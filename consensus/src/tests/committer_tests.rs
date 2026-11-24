use super::*;
use crate::common::{dag_block, keys, NODES};
use rand::rngs::StdRng;
use rand::seq::SliceRandom as _;
use rand::SeedableRng as _;

const GC_DEPTH: SeqNumber = 6;

struct TestDag {
    dag: Dag,
    // (round, author index) -> block
    blocks: HashMap<(SeqNumber, usize), (Digest, Block)>,
    authors: Vec<PublicKey>,
}

impl TestDag {
    // Builds `waves` waves. `parents(round, author)` lists the author indices of
    // the previous round referenced by the block; r2 blocks always reference
    // (and carry the QC1 of) their author's r1 block.
    fn new<F>(waves: SeqNumber, mut parents: F) -> Self
    where
        F: FnMut(SeqNumber, usize) -> Vec<usize>,
    {
        let authors: Vec<PublicKey> = keys().into_iter().map(|(name, _)| name).collect();
        let mut dag = Dag::new();
        let mut blocks: HashMap<(SeqNumber, usize), (Digest, Block)> = HashMap::new();
        for round in 1..=round_of(waves, 3) {
            for author in 0..NODES {
                let mut indices = if round == 1 { Vec::new() } else { parents(round, author) };
                let own_r1 = if crate::messages::phase_of(round) == R2 {
                    if !indices.contains(&author) {
                        indices.push(author);
                    }
                    Some(blocks[&(round - 1, author)].0.clone())
                } else {
                    None
                };
                let refs: Vec<(Digest, &Block)> = indices
                    .iter()
                    .map(|i| {
                        let (digest, block) = &blocks[&(round - 1, *i)];
                        (digest.clone(), block)
                    })
                    .collect();
                let (digest, block) = dag_block(authors[author], round, &refs, &[], own_r1);
                dag.insert(digest.clone(), block.clone());
                blocks.insert((round, author), (digest, block));
            }
        }
        Self {
            dag,
            blocks,
            authors,
        }
    }

    fn full(waves: SeqNumber) -> Self {
        Self::new(waves, |_, _| (0..NODES).collect())
    }

    fn leader_index(wave: SeqNumber) -> usize {
        wave as usize % NODES
    }

    fn leader_of(&self, wave: SeqNumber) -> Option<PublicKey> {
        Some(self.authors[Self::leader_index(wave)])
    }

    // The leader block (r1) of the wave.
    fn anchor(&self, wave: SeqNumber) -> Digest {
        self.blocks[&(round_of(wave, R1), Self::leader_index(wave))].0.clone()
    }

    fn leader_r2(&self, wave: SeqNumber) -> Digest {
        self.blocks[&(round_of(wave, R2), Self::leader_index(wave))].0.clone()
    }

    // What a direct commit guarantees: every r1 block of the next wave has a
    // strong path to the leader's r2 block.
    fn reached_by_next_wave(&self, wave: SeqNumber) -> bool {
        (0..NODES).all(|author| {
            let (digest, _) = &self.blocks[&(round_of(wave + 1, R1), author)];
            self.dag
                .strong_descend([digest.clone()].into_iter().collect(), round_of(wave + 1, R1), round_of(wave, R2))
                .contains(&self.leader_r2(wave))
        })
    }
}

fn commit(committer: &mut Committer, test: &TestDag, dag: &Dag, wave: SeqNumber) -> Vec<CommittedSubDag> {
    committer
        .commit(dag, &test.anchor(wave), |w| test.leader_of(w))
        .expect("all leaders are known")
}

fn sequence(sub_dags: &[CommittedSubDag]) -> Vec<Digest> {
    sub_dags
        .iter()
        .flat_map(|sub_dag| sub_dag.blocks.iter().map(|(digest, _)| digest.clone()))
        .collect()
}

#[test]
fn commit_orders_causal_history() {
    let test = TestDag::full(2);
    let mut committer = Committer::new(GC_DEPTH);

    // The first leader block (round 1) has no history.
    let sub_dags = commit(&mut committer, &test, &test.dag, 1);
    assert_eq!(sequence(&sub_dags), vec![test.anchor(1)]);
    assert_eq!(committer.last_committed_wave(), 1);

    // The second one commits rounds 1..3 (but the first leader) and itself,
    // ordered by (round, author).
    let sub_dags = commit(&mut committer, &test, &test.dag, 2);
    assert_eq!(sub_dags.len(), 1);
    let blocks = &sub_dags[0].blocks;
    assert_eq!(blocks.len(), 3 + 4 + 4 + 1);
    assert_eq!(blocks.last().unwrap().0, test.anchor(2));
    let keys: Vec<_> = blocks.iter().map(|(_, b)| (b.height, b.author)).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn direct_and_indirect_commits_agree() {
    let test = TestDag::full(3);

    // Node A commits every leader directly.
    let mut a = Committer::new(GC_DEPTH);
    let mut sequence_a = Vec::new();
    for wave in 1..=3 {
        sequence_a.extend(sequence(&commit(&mut a, &test, &test.dag, wave)));
    }

    // Node B only commits the last leader directly and reaches the others
    // through the leader chain.
    let mut b = Committer::new(GC_DEPTH);
    let sub_dags = commit(&mut b, &test, &test.dag, 3);
    let waves: Vec<_> = sub_dags.iter().map(|sub_dag| sub_dag.wave).collect();
    assert_eq!(waves, vec![1, 2, 3]);
    assert_eq!(sequence(&sub_dags), sequence_a);
}

#[test]
fn leader_without_strong_path_is_skipped() {
    // No r3 block of wave 1 references the r2 block of the wave-1 leader.
    let leader = TestDag::leader_index(1);
    let test = TestDag::new(2, |round, _| {
        if round == round_of(1, 3) {
            (0..NODES).filter(|i| *i != leader).collect()
        } else {
            (0..NODES).collect()
        }
    });
    let mut committer = Committer::new(GC_DEPTH);
    let sub_dags = commit(&mut committer, &test, &test.dag, 2);
    let waves: Vec<_> = sub_dags.iter().map(|sub_dag| sub_dag.wave).collect();
    assert_eq!(waves, vec![2]);
    let sequence = sequence(&sub_dags);
    assert!(!sequence.contains(&test.leader_r2(1)));
    // The skipped leader block is still committed as part of the history.
    assert!(sequence.contains(&test.anchor(1)));
}

#[test]
fn missing_leader_keeps_state() {
    let test = TestDag::full(2);
    let mut committer = Committer::new(GC_DEPTH);
    let result = committer.commit(&test.dag, &test.anchor(2), |wave| {
        if wave == 1 {
            None
        } else {
            test.leader_of(wave)
        }
    });
    assert!(result.is_none());
    assert_eq!(committer.last_committed_wave(), 0);

    let sub_dags = commit(&mut committer, &test, &test.dag, 2);
    assert_eq!(sub_dags.len(), 2);
}

#[test]
fn random_dags_agree_regardless_of_commit_and_gc_timing() {
    for seed in 0..10u8 {
        // Every block references its author's previous block and two others.
        let mut rng = StdRng::from_seed([seed; 32]);
        let waves = 30;
        let test = TestDag::new(waves, |_, author| {
            let others: Vec<usize> = (0..NODES).filter(|i| *i != author).collect();
            let mut parents: Vec<usize> = others.choose_multiple(&mut rng, 2).cloned().collect();
            parents.push(author);
            parents
        });

        // Node A directly commits every leader for which a direct commit is
        // possible, and garbage collects as it goes.
        let mut dag_a = Dag::new();
        for (digest, block) in test.blocks.values() {
            dag_a.insert(digest.clone(), block.clone());
        }
        let mut a = Committer::new(GC_DEPTH);
        let mut sequence_a = Vec::new();
        let mut direct = 0;
        for wave in 1..waves {
            if test.reached_by_next_wave(wave) {
                direct += 1;
                sequence_a.extend(sequence(&commit(&mut a, &test, &dag_a, wave)));
                dag_a.gc(a.gc_round());
            }
        }
        sequence_a.extend(sequence(&commit(&mut a, &test, &dag_a, waves)));

        // Node B commits the last leader only, without garbage collection.
        let mut b = Committer::new(GC_DEPTH);
        let sequence_b = sequence(&commit(&mut b, &test, &test.dag, waves));

        assert!(direct > 0);
        assert_eq!(sequence_a, sequence_b, "seed {}", seed);
        let unique: HashSet<_> = sequence_a.iter().collect();
        assert_eq!(unique.len(), sequence_a.len());
    }
}
