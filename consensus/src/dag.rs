use crate::core::SeqNumber;
use crate::messages::Block;
use crypto::{Digest, PublicKey};
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg(test)]
#[path = "tests/dag_tests.rs"]
pub mod dag_tests;

// The local view of the DAG. Only crash faults are considered, so there is at
// most one block per (round, author).
#[derive(Default)]
pub struct Dag {
    rounds: BTreeMap<SeqNumber, HashMap<PublicKey, (Digest, Block)>>,
    index: HashMap<Digest, (SeqNumber, PublicKey)>,
    gc_round: SeqNumber,
}

impl Dag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn gc_round(&self) -> SeqNumber {
        self.gc_round
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        self.index.contains_key(digest)
    }

    pub fn get(&self, digest: &Digest) -> Option<&Block> {
        let (round, author) = self.index.get(digest)?;
        self.get_at(*round, author).map(|(_, block)| block)
    }

    pub fn get_at(&self, round: SeqNumber, author: &PublicKey) -> Option<&(Digest, Block)> {
        self.rounds.get(&round)?.get(author)
    }

    pub fn round(&self, round: SeqNumber) -> impl Iterator<Item = &(Digest, Block)> {
        self.rounds.get(&round).into_iter().flat_map(|x| x.values())
    }

    // References are only checked above the GC horizon: older parents may
    // already be cleaned up.
    pub fn missing_parents(&self, block: &Block) -> Vec<(Digest, SeqNumber)> {
        block
            .references
            .iter()
            .filter(|(_, digest, round)| *round >= self.gc_round && !self.contains(digest))
            .map(|(_, digest, round)| (digest.clone(), *round))
            .collect()
    }

    pub fn insert(&mut self, digest: Digest, block: Block) {
        if block.height < self.gc_round || self.contains(&digest) {
            return;
        }
        self.index.insert(digest.clone(), (block.height, block.author));
        self.rounds
            .entry(block.height)
            .or_insert_with(HashMap::new)
            .insert(block.author, (digest, block));
    }

    // The blocks of round `target` reachable from `from` (blocks of round
    // `from_round`) through strong references only.
    pub fn strong_descend(
        &self,
        from: HashSet<Digest>,
        from_round: SeqNumber,
        target: SeqNumber,
    ) -> HashSet<Digest> {
        let mut frontier = from;
        let mut round = from_round;
        while round > target && !frontier.is_empty() {
            frontier = frontier
                .iter()
                .filter_map(|digest| self.get(digest))
                .flat_map(|block| block.strong_references().cloned())
                .collect();
            round -= 1;
        }
        frontier
    }

    // The causal history of `from` (strong and weak references), restricted to
    // rounds >= `min_round` and not entering blocks for which `stop` holds.
    pub fn causal_history<F>(&self, from: &[Digest], min_round: SeqNumber, stop: F) -> Vec<Digest>
    where
        F: Fn(&Digest) -> bool,
    {
        let mut visited = HashSet::new();
        let mut stack: Vec<Digest> = from.to_vec();
        let mut history = Vec::new();
        while let Some(digest) = stack.pop() {
            if stop(&digest) || !visited.insert(digest.clone()) {
                continue;
            }
            let block = match self.get(&digest) {
                Some(block) => block,
                None => continue,
            };
            for (_, parent, round) in &block.references {
                if *round >= min_round && !visited.contains(parent) {
                    stack.push(parent.clone());
                }
            }
            history.push(digest);
        }
        history
    }

    pub fn gc(&mut self, gc_round: SeqNumber) {
        if gc_round <= self.gc_round {
            return;
        }
        self.gc_round = gc_round;
        self.rounds = self.rounds.split_off(&gc_round);
        self.index.retain(|_, (round, _)| *round >= gc_round);
    }
}
