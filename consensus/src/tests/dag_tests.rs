use super::*;
use crate::common::{dag_block, keys};
use crate::messages::Block;

fn authors() -> Vec<PublicKey> {
    keys().into_iter().map(|(name, _)| name).collect()
}

// Rounds 1..=rounds where every block references all blocks of the previous round.
fn full_dag(rounds: SeqNumber) -> (Dag, Vec<Vec<(Digest, Block)>>) {
    let mut dag = Dag::new();
    let mut layers: Vec<Vec<(Digest, Block)>> = vec![Vec::new()];
    for round in 1..=rounds {
        let parents: Vec<(Digest, &Block)> = layers[round as usize - 1]
            .iter()
            .map(|(digest, block)| (digest.clone(), block))
            .collect();
        let layer: Vec<_> = authors()
            .into_iter()
            .map(|author| dag_block(author, round, &parents, &[], None))
            .collect();
        for (digest, block) in &layer {
            dag.insert(digest.clone(), block.clone());
        }
        layers.push(layer);
    }
    (dag, layers)
}

#[test]
fn insert_and_get() {
    let (dag, layers) = full_dag(2);
    for (digest, block) in layers[2].iter().chain(layers[1].iter()) {
        assert!(dag.contains(digest));
        assert_eq!(dag.get(digest).unwrap().author, block.author);
        assert_eq!(&dag.get_at(block.height, &block.author).unwrap().0, digest);
    }
    assert_eq!(dag.round(2).count(), 4);
    assert_eq!(dag.round(3).count(), 0);
}

#[test]
fn missing_parents() {
    let (_, layers) = full_dag(2);
    let mut dag = Dag::new();
    for (digest, block) in layers[1].iter().skip(1) {
        dag.insert(digest.clone(), block.clone());
    }
    let (_, child) = &layers[2][0];
    assert_eq!(dag.missing_parents(child), vec![(layers[1][0].0.clone(), 1)]);
}

#[test]
fn strong_descend_ignores_weak_references() {
    let (_, layers) = full_dag(1);
    let authors = authors();
    let mut dag = Dag::new();
    for (digest, block) in &layers[1] {
        dag.insert(digest.clone(), block.clone());
    }
    // Round 2 blocks only reference authors 0..2 of round 1.
    let parents: Vec<_> = layers[1][..3].iter().map(|(d, b)| (d.clone(), b)).collect();
    let round_2: Vec<_> = authors
        .iter()
        .map(|author| dag_block(*author, 2, &parents, &[], None))
        .collect();
    for (digest, block) in &round_2 {
        dag.insert(digest.clone(), block.clone());
    }
    // Round 3 block: strong references to round 2, and a weak reference to
    // the round 1 block of author 3.
    let parents: Vec<_> = round_2.iter().map(|(d, b)| (d.clone(), b)).collect();
    let weak = vec![(layers[1][3].0.clone(), &layers[1][3].1)];
    let (digest, block) = dag_block(authors[0], 3, &parents, &weak, None);
    dag.insert(digest.clone(), block);

    let reached = dag.strong_descend([digest.clone()].into_iter().collect(), 3, 1);
    let expected: HashSet<_> = layers[1][..3].iter().map(|(d, _)| d.clone()).collect();
    assert_eq!(reached, expected);

    // The causal history follows weak references too.
    let history = dag.causal_history(&[digest], 1, |_| false);
    assert_eq!(history.len(), 1 + 4 + 4);
    assert!(history.contains(&layers[1][3].0));
}

#[test]
fn causal_history_respects_bounds() {
    let (dag, layers) = full_dag(3);
    let top = layers[3][0].0.clone();
    assert_eq!(dag.causal_history(&[top.clone()], 1, |_| false).len(), 1 + 4 + 4);
    assert_eq!(dag.causal_history(&[top.clone()], 2, |_| false).len(), 1 + 4);

    // Blocks for which `stop` holds are neither included nor expanded.
    let stopped: HashSet<_> = layers[2].iter().map(|(d, _)| d.clone()).collect();
    let history = dag.causal_history(&[top], 1, |d| stopped.contains(d));
    assert_eq!(history.len(), 1);
}

#[test]
fn garbage_collection() {
    let (mut dag, layers) = full_dag(3);
    dag.gc(2);
    assert_eq!(dag.gc_round(), 2);
    assert!(layers[1].iter().all(|(d, _)| !dag.contains(d)));
    assert!(layers[2].iter().all(|(d, _)| dag.contains(d)));

    // Parents below the GC round are no longer required.
    let (_, child) = &layers[2][0];
    assert!(dag.missing_parents(child).is_empty());

    // Blocks below the GC round are not inserted.
    let (digest, block) = layers[1][0].clone();
    dag.insert(digest.clone(), block);
    assert!(!dag.contains(&digest));
}
