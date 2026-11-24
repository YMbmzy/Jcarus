use super::*;
use crate::common::{block, committee, keys, qc, qc_for, tss_keys, vote};
use crypto::generate_keypair;
use rand::rngs::StdRng;
use rand::SeedableRng as _;

#[test]
fn round_arithmetic() {
    assert_eq!((wave_of(1), phase_of(1)), (1, R1));
    assert_eq!((wave_of(3), phase_of(3)), (1, R3));
    assert_eq!((wave_of(4), phase_of(4)), (2, R1));
    assert_eq!((wave_of(8), phase_of(8)), (3, R2));
    for round in 1..30 {
        assert_eq!(round_of(wave_of(round), phase_of(round)), round);
    }
}

#[test]
fn verify_valid_qc() {
    assert!(qc().verify(&committee()).is_ok());
}

#[test]
fn verify_qc_authority_reuse() {
    // Modify QC to reuse one authority.
    let mut qc = qc();
    let _ = qc.votes.pop();
    let vote = qc.votes[0].clone();
    qc.votes.push(vote.clone());

    // Verify the QC.
    match qc.verify(&committee()) {
        Err(ConsensusError::AuthorityReuseinQC(name)) => assert_eq!(name, vote.0),
        _ => assert!(false),
    }
}

#[test]
fn verify_qc_unknown_authority() {
    let mut qc = qc();

    // Modify QC to add one unknown authority.
    let mut rng = StdRng::from_seed([1; 32]);
    let (unknown, _) = generate_keypair(&mut rng);
    let (_, sig) = qc.votes.pop().unwrap();
    qc.votes.push((unknown, sig));

    // Verify the QC.
    match qc.verify(&committee()) {
        Err(ConsensusError::UnknownAuthority(name)) => assert_eq!(name, unknown),
        _ => assert!(false),
    }
}

#[test]
fn verify_qc_insufficient_stake() {
    // Modify QC to remove one authority.
    let mut qc = qc();
    let _ = qc.votes.pop();

    // Verify the QC.
    match qc.verify(&committee()) {
        Err(ConsensusError::QCRequiresQuorum) => assert!(true),
        _ => assert!(false),
    }
}

#[test]
fn votes_and_qc_sign_the_same_digest() {
    let vote = vote();
    let qc = QC {
        hash: vote.hash.clone(),
        epoch: vote.epoch,
        height: vote.height,
        proposer: vote.proposer,
        votes: Vec::new(),
    };
    assert_eq!(vote.digest(), qc.digest());
}

#[test]
fn verify_block() {
    assert!(block().verify(&committee()).is_ok());

    // An r2 block must carry a valid QC1.
    let (author, secret) = keys().pop().unwrap();
    let r1 = block();
    let qc1 = qc_for(r1.digest(), 1, author);
    let references = vec![(author, r1.digest(), 1)];
    let r2 = Block::new_from_key(qc1, author, 2, Vec::new(), references.clone(), &secret);
    assert!(r2.verify(&committee()).is_ok());

    let mut bad_qc1 = qc_for(r1.digest(), 1, author);
    let _ = bad_qc1.votes.pop();
    let r2 = Block::new_from_key(bad_qc1, author, 2, Vec::new(), references, &secret);
    assert!(r2.verify(&committee()).is_err());

    // The signature must match the content.
    let mut tampered = block();
    tampered.height = 4;
    assert!(tampered.verify(&committee()).is_err());
}

#[test]
fn verify_coin_share() {
    let committee = committee();
    let tss = tss_keys();
    let pk_set = tss[0].pkset.clone();
    let (author, _) = keys()[1];
    let share = CoinShare {
        wave: 3,
        author,
        share: tss[1].secret.sign(CoinShare::coin_digest(3)),
    };
    assert!(share.verify(&committee, &pk_set).is_ok());

    // A share of another node does not verify.
    let share = CoinShare {
        share: tss[2].secret.sign(CoinShare::coin_digest(3)),
        ..share
    };
    assert!(share.verify(&committee, &pk_set).is_err());
}
