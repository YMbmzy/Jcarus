use crate::config::Committee;
use crate::core::{ConsensusMessage, SeqNumber, BLOCK_CHANNEL};
use crate::error::ConsensusResult;
use crate::filter::FilterInput;
use crypto::{Digest, PublicKey};
use log::debug;
use std::collections::HashMap;
use tokio::sync::mpsc::Sender;
use tokio::time::{Duration, Instant};

#[cfg(test)]
#[path = "tests/synchronizer_tests.rs"]
pub mod synchronizer_tests;

// Requests blocks that are referenced but still missing after `sync_retry_delay`.
// Parents usually arrive by themselves shortly after their children, so nothing
// is requested before that delay.
pub struct Synchronizer {
    name: PublicKey,
    committee: Committee,
    network_filter: Sender<FilterInput>,
    sync_retry_delay: Duration,
    // Missing digest -> (round, node to ask first, deadline, already requested).
    missing: HashMap<Digest, (SeqNumber, PublicKey, Instant, bool)>,
}

impl Synchronizer {
    pub fn new(
        name: PublicKey,
        committee: Committee,
        network_filter: Sender<FilterInput>,
        sync_retry_delay: u64,
    ) -> Self {
        Self {
            name,
            committee,
            network_filter,
            sync_retry_delay: Duration::from_millis(sync_retry_delay),
            missing: HashMap::new(),
        }
    }

    // `origin` references the block, so it should have it.
    pub fn add_missing(&mut self, digest: Digest, round: SeqNumber, origin: PublicKey) {
        let deadline = Instant::now() + self.sync_retry_delay;
        self.missing
            .entry(digest)
            .or_insert((round, origin, deadline, false));
    }

    pub fn received(&mut self, digest: &Digest) {
        self.missing.remove(digest);
    }

    // Asks the referencing node first, and every node on later attempts.
    pub async fn request_expired(&mut self) -> ConsensusResult<()> {
        let now = Instant::now();
        let mut targeted: HashMap<PublicKey, Vec<Digest>> = HashMap::new();
        let mut broadcast = Vec::new();
        for (digest, (_, origin, deadline, requested)) in self.missing.iter_mut() {
            if *deadline > now {
                continue;
            }
            if *requested {
                broadcast.push(digest.clone());
            } else {
                targeted.entry(*origin).or_insert_with(Vec::new).push(digest.clone());
            }
            *deadline = now + self.sync_retry_delay;
            *requested = true;
        }
        for (origin, digests) in targeted {
            debug!("Requesting sync for {} blocks from {}", digests.len(), origin);
            let message = ConsensusMessage::SyncRequest(digests, self.name);
            Self::transmit(message, &self.name, Some(&origin), &self.network_filter, &self.committee, BLOCK_CHANNEL).await?;
        }
        if !broadcast.is_empty() {
            debug!("Requesting sync for {} blocks (retry)", broadcast.len());
            let message = ConsensusMessage::SyncRequest(broadcast, self.name);
            Self::transmit(message, &self.name, None, &self.network_filter, &self.committee, BLOCK_CHANNEL).await?;
        }
        Ok(())
    }

    pub fn cleanup(&mut self, gc_round: SeqNumber) {
        self.missing.retain(|_, (round, _, _, _)| *round >= gc_round);
    }

    pub async fn transmit(
        message: ConsensusMessage,
        from: &PublicKey,
        to: Option<&PublicKey>,
        network_filter: &Sender<FilterInput>,
        committee: &Committee,
        channel: u8,
    ) -> ConsensusResult<()> {
        let addresses = if let Some(to) = to {
            debug!("Sending {:?} to {}", message, to);
            if channel == BLOCK_CHANNEL {
                vec![committee.address(to)?]
            } else {
                vec![committee.smvba_address(to)?]
            }
        } else {
            debug!("Broadcasting {:?}", message);
            if channel == BLOCK_CHANNEL {
                committee.broadcast_addresses(from)
            } else {
                committee.smvba_broadcast_addresses(from)
            }
        };
        if let Err(e) = network_filter.send((message, addresses)).await {
            panic!("Failed to send block through network channel: {}", e);
        }
        Ok(())
    }
}
