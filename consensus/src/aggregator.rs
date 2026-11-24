use crate::config::{Committee, Stake};
use crate::core::SeqNumber;
use crate::error::{ConsensusError, ConsensusResult};
use crate::messages::{CoinShare, Vote, QC};
use crypto::{PublicKey, Signature};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::TryInto;
use threshold_crypto::PublicKeySet;

#[cfg(test)]
#[path = "tests/aggregator_tests.rs"]
pub mod aggregator_tests;

pub struct Aggregator {
    committee: Committee,
    // Votes for our own blocks, keyed by round (we author one block per round).
    votes_aggregators: HashMap<SeqNumber, Box<QCMaker>>,
    // Coin shares, keyed by wave.
    coin_aggregators: HashMap<SeqNumber, Box<CoinMaker>>,
}

impl Aggregator {
    pub fn new(committee: Committee) -> Self {
        Self {
            committee,
            votes_aggregators: HashMap::new(),
            coin_aggregators: HashMap::new(),
        }
    }

    pub fn add_vote(&mut self, vote: Vote) -> ConsensusResult<Option<QC>> {
        // Add the new vote to our aggregator and see if we have a QC.
        self.votes_aggregators
            .entry(vote.height)
            .or_insert_with(|| Box::new(QCMaker::new()))
            .append(vote, &self.committee)
    }

    // Returns the coin value once f+1 shares of the wave are collected.
    pub fn add_coin_share(
        &mut self,
        share: CoinShare,
        pk_set: &PublicKeySet,
    ) -> ConsensusResult<Option<usize>> {
        self.coin_aggregators
            .entry(share.wave)
            .or_insert_with(|| Box::new(CoinMaker::new()))
            .append(share, &self.committee, pk_set)
    }

    pub fn cleanup(&mut self, gc_round: SeqNumber, gc_wave: SeqNumber) {
        self.votes_aggregators.retain(|k, _| k >= &gc_round);
        self.coin_aggregators.retain(|k, _| k >= &gc_wave);
    }
}

struct QCMaker {
    weight: Stake,
    votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
}

impl QCMaker {
    pub fn new() -> Self {
        Self {
            weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
        }
    }

    /// Try to append a signature to a (partial) quorum.
    pub fn append(&mut self, vote: Vote, committee: &Committee) -> ConsensusResult<Option<QC>> {
        let author = vote.author;

        // Ensure it is the first time this authority votes.
        ensure!(
            self.used.insert(author),
            ConsensusError::AuthorityReuseinQC(author)
        );

        self.votes.push((author, vote.signature));
        self.weight += committee.stake(&author);

        if self.weight >= committee.quorum_threshold() {
            self.weight = 0; // Ensures QC is only made once.
            return Ok(Some(QC {
                hash: vote.hash,
                epoch: vote.epoch,
                height: vote.height,
                proposer: vote.proposer,
                votes: self.votes.clone(),
            }));
        }
        Ok(None)
    }
}

struct CoinMaker {
    weight: Stake,
    shares: BTreeMap<usize, threshold_crypto::SignatureShare>,
    used: HashSet<PublicKey>,
}

impl CoinMaker {
    pub fn new() -> Self {
        Self {
            weight: 0,
            shares: BTreeMap::new(),
            used: HashSet::new(),
        }
    }

    /// Try to append a share to a (partial) set of f+1 shares.
    pub fn append(
        &mut self,
        share: CoinShare,
        committee: &Committee,
        pk_set: &PublicKeySet,
    ) -> ConsensusResult<Option<usize>> {
        let author = share.author;

        // Ensure it is the first time this authority sends its share.
        ensure!(
            self.used.insert(author),
            ConsensusError::AuthorityReuseinCoin(author)
        );

        self.shares.insert(committee.id(author), share.share);
        self.weight += committee.stake(&author);

        // Combine exactly once, when the threshold is reached.
        if self.weight >= committee.random_coin_threshold()
            && self.weight - committee.stake(&author) < committee.random_coin_threshold()
        {
            let sig = pk_set
                .combine_signatures(self.shares.iter())
                .map_err(|_| ConsensusError::RandomCoinWithWrongShares)?;
            let coin = u64::from_be_bytes(sig.to_bytes()[0..8].try_into().unwrap());
            return Ok(Some(coin as usize));
        }
        Ok(None)
    }
}
