use crate::core::SeqNumber;
use crate::dag::Dag;
use crate::messages::{round_of, Block, R1, R2};
use crypto::{Digest, PublicKey};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
#[path = "tests/committer_tests.rs"]
pub mod committer_tests;

// The blocks committed with one leader, in commit order.
pub struct CommittedSubDag {
    pub wave: SeqNumber,
    pub leader: Digest,
    pub blocks: Vec<(Digest, Block)>,
}

pub struct Committer {
    gc_depth: SeqNumber,
    last_committed_wave: SeqNumber,
    last_leader_round: SeqNumber,
    committed: HashMap<Digest, SeqNumber>,
}

impl Committer {
    pub fn new(gc_depth: SeqNumber) -> Self {
        Self {
            gc_depth,
            last_committed_wave: 0,
            last_leader_round: 0,
            committed: HashMap::new(),
        }
    }

    pub fn last_committed_wave(&self) -> SeqNumber {
        self.last_committed_wave
    }

    pub fn is_committed(&self, digest: &Digest) -> bool {
        self.committed.contains_key(digest)
    }

    // Blocks below this round are never needed again by the committer.
    pub fn gc_round(&self) -> SeqNumber {
        self.last_leader_round.saturating_sub(self.gc_depth)
    }

    // Commits the leader block `anchor` (the r1 block of its wave leader) and,
    // first, every uncommitted earlier leader it reaches through the leader chain.
    // Returns None (without changing any state) if the leader of a wave on the
    // way is not yet known.
    pub fn commit<F>(&mut self, dag: &Dag, anchor: &Digest, leader_of: F) -> Option<Vec<CommittedSubDag>>
    where
        F: Fn(SeqNumber) -> Option<PublicKey>,
    {
        let anchor_wave = dag.get(anchor)?.wave();
        if anchor_wave <= self.last_committed_wave {
            return Some(Vec::new());
        }

        // Walk back the leader chain: the leader of wave w is included iff the
        // latest included leader has a strong path to its r2 block, whose QC1
        // names the leader block of wave w.
        let mut leaders = vec![(anchor_wave, anchor.clone())];
        let mut frontier: HashSet<Digest> = [anchor.clone()].into_iter().collect();
        let mut frontier_round = round_of(anchor_wave, R1);
        for wave in (self.last_committed_wave + 1..anchor_wave).rev() {
            let leader = leader_of(wave)?;
            let target = round_of(wave, R2);
            frontier = dag.strong_descend(frontier, frontier_round, target);
            frontier_round = target;
            if let Some((digest, block)) = dag.get_at(target, &leader) {
                if frontier.contains(digest) {
                    leaders.push((wave, block.qc.hash.clone()));
                    frontier = [block.qc.hash.clone()].into_iter().collect();
                    frontier_round = round_of(wave, R1);
                }
            }
        }

        // Commit the leaders by increasing wave, each with its causal history.
        let mut sub_dags = Vec::new();
        for (wave, leader) in leaders.into_iter().rev() {
            let leader_round = round_of(wave, R1);
            let min_round = leader_round.saturating_sub(self.gc_depth);
            let history = dag.causal_history(&[leader.clone()], min_round, |digest| {
                self.committed.contains_key(digest)
            });
            let mut blocks: Vec<(Digest, Block)> = history
                .into_iter()
                .filter_map(|digest| dag.get(&digest).cloned().map(|block| (digest, block)))
                .collect();
            blocks.sort_by(|(_, a), (_, b)| (a.height, a.author).cmp(&(b.height, b.author)));
            for (digest, block) in &blocks {
                self.committed.insert(digest.clone(), block.height);
            }
            self.last_committed_wave = wave;
            self.last_leader_round = leader_round;
            sub_dags.push(CommittedSubDag {
                wave,
                leader,
                blocks,
            });
        }

        let gc_round = self.gc_round();
        self.committed.retain(|_, round| *round >= gc_round);
        Some(sub_dags)
    }
}
