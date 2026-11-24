use super::*;
use crate::common::{committee, keys, qc, tss_keys, vote};
use crypto::Hash;

#[test]
fn add_vote() {
    let mut aggregator = Aggregator::new(committee());
    let result = aggregator.add_vote(vote());
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn make_qc_once() {
    let mut aggregator = Aggregator::new(committee());
    let qc = qc();
    let hash = qc.digest();
    let height = qc.height;
    let proposer = qc.proposer;

    // Add 2f+1 votes to the aggregator and ensure it returns the cryptographic
    // material to make a valid QC, exactly once.
    let mut made = Vec::new();
    for (public_key, secret_key) in keys() {
        let vote = Vote::new_from_key(hash.clone(), height, proposer, public_key, &secret_key);
        if let Some(qc) = aggregator.add_vote(vote).unwrap() {
            made.push(qc);
        }
    }
    assert_eq!(made.len(), 1);
    assert!(made[0].verify(&committee()).is_ok());
}

#[test]
fn duplicate_vote_error() {
    let mut aggregator = Aggregator::new(committee());
    let vote = vote();

    // First vote should succeed
    let result = aggregator.add_vote(vote.clone());
    assert!(result.is_ok());

    // Duplicate vote should return error
    let result = aggregator.add_vote(vote);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), ConsensusError::AuthorityReuseinQC(_)));
}

fn coin_from(nodes: &[usize], wave: SeqNumber) -> Vec<Option<usize>> {
    let mut aggregator = Aggregator::new(committee());
    let tss = tss_keys();
    let pk_set = tss[0].pkset.clone();
    let keys = keys();
    nodes
        .iter()
        .map(|i| {
            let share = CoinShare {
                wave,
                author: keys[*i].0,
                share: tss[*i].secret.sign(CoinShare::coin_digest(wave)),
            };
            aggregator.add_coin_share(share, &pk_set).unwrap()
        })
        .collect()
}

#[test]
fn coin_revealed_by_f_plus_one_shares() {
    // f+1 = 2 shares reveal the coin, once.
    let results = coin_from(&[0, 1, 2], 1);
    assert!(results[0].is_none());
    assert!(results[1].is_some());
    assert!(results[2].is_none());
}

#[test]
fn coin_is_the_same_for_any_shares() {
    let a = coin_from(&[0, 1], 5)[1].unwrap();
    let b = coin_from(&[3, 2], 5)[1].unwrap();
    let c = coin_from(&[1, 3], 5)[1].unwrap();
    assert_eq!(a, b);
    assert_eq!(a, c);
}
