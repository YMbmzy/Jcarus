use crate::aggregator::Aggregator;
use crate::committer::{CommittedSubDag, Committer};
use crate::config::{Committee, Parameters, Stake};
use crate::dag::Dag;
use crate::error::{ConsensusError, ConsensusResult};
use crate::filter::FilterInput;
use crate::leader::LeaderElector;
use crate::mempool::MempoolDriver;
use crate::messages::{phase_of, round_of, wave_of, Block, Certificate, CoinShare, Vote, QC, R1, R2, R3};
use crate::synchronizer::Synchronizer;
use crypto::Hash as _;
use crypto::{Digest, PublicKey, SignatureService};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::cmp::min;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use store::Store;
use threshold_crypto::PublicKeySet;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{interval, sleep_until, Duration, Instant};

pub type SeqNumber = u64;

// Blocks travel on the consensus port. Votes and certificates use the second
// (former SMVBA) port so that they are not queued behind blocks.
pub const BLOCK_CHANNEL: u8 = 0;
pub const FAST_CHANNEL: u8 = 1;

// Number of rounds kept below the latest committed leader. Garbage collection
// is disabled: with it, the others ignore the blocks of a node lagging more
// than GC_DEPTH rounds behind, so it can never certify its r1 block again and
// stops proposing. Benchmark runs are short, and committed payloads still
// leave the mempool queues.
pub const GC_DEPTH: SeqNumber = SeqNumber::MAX;

#[derive(Serialize, Deserialize, Debug)]
pub enum ConsensusMessage {
    Propose(Block),
    Vote(Vote),
    Certificate(Certificate),
    LoopBack(Block),
    SyncRequest(Vec<Digest>, PublicKey),
    SyncReply(Block),
}

pub struct Core {
    name: PublicKey,
    committee: Committee,
    parameters: Parameters,
    store: Store,
    signature_service: SignatureService,
    pk_set: PublicKeySet,
    leader_elector: LeaderElector,
    mempool_driver: MempoolDriver,
    synchronizer: Synchronizer,
    core_channel: Receiver<ConsensusMessage>,
    fast_channel: Receiver<ConsensusMessage>,
    network_filter: Sender<FilterInput>,
    network_filter_fast: Sender<FilterInput>,
    commit_channel: Sender<Block>,
    aggregator: Aggregator,
    dag: Dag,
    committer: Committer,
    // Blocks waiting for their parents, with the number of missing parents.
    pending: HashMap<Digest, (Block, usize)>,
    // Missing parent -> blocks waiting for it.
    waiting: HashMap<Digest, Vec<Digest>>,
    last_proposed_round: SeqNumber,
    last_proposal_time: Instant,
    proposal_deadline: Option<Instant>,
    my_blocks: HashMap<SeqNumber, Digest>, // round -> our block
    my_qc1: HashMap<SeqNumber, QC>, // wave -> QC1 of our r1 block
    qc2s: HashMap<(SeqNumber, PublicKey), Digest>, // (wave, author) -> r2 block with a QC2
    leaders: BTreeMap<SeqNumber, PublicKey>, // wave -> leader
    // Certified blocks (QC1 or QC2) of other nodes not yet covered by our blocks.
    weak_candidates: BTreeSet<(SeqNumber, PublicKey)>,
    commit_index: u64,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: PublicKey,
        committee: Committee,
        parameters: Parameters,
        signature_service: SignatureService,
        pk_set: PublicKeySet,
        store: Store,
        leader_elector: LeaderElector,
        mempool_driver: MempoolDriver,
        synchronizer: Synchronizer,
        core_channel: Receiver<ConsensusMessage>,
        fast_channel: Receiver<ConsensusMessage>,
        network_filter: Sender<FilterInput>,
        network_filter_fast: Sender<FilterInput>,
        commit_channel: Sender<Block>,
    ) -> Self {
        let aggregator = Aggregator::new(committee.clone());
        Self {
            name,
            committee,
            parameters,
            store,
            signature_service,
            pk_set,
            leader_elector,
            mempool_driver,
            synchronizer,
            core_channel,
            fast_channel,
            network_filter,
            network_filter_fast,
            commit_channel,
            aggregator,
            dag: Dag::new(),
            committer: Committer::new(GC_DEPTH),
            pending: HashMap::new(),
            waiting: HashMap::new(),
            last_proposed_round: 0,
            last_proposal_time: Instant::now(),
            proposal_deadline: None,
            my_blocks: HashMap::new(),
            my_qc1: HashMap::new(),
            qc2s: HashMap::new(),
            leaders: BTreeMap::new(),
            weak_candidates: BTreeSet::new(),
            commit_index: 0,
        }
    }

    async fn store_block(&mut self, digest: &Digest, block: &Block) {
        let value = bincode::serialize(block).expect("Failed to serialize block");
        self.store.write(digest.to_vec(), value).await;
    }

    fn has_quorum(&self, round: SeqNumber) -> bool {
        let stake: Stake = self
            .dag
            .round(round)
            .map(|(_, block)| self.committee.stake(&block.author))
            .sum();
        stake >= self.committee.quorum_threshold()
    }

    // Proposes the next rounds, one by one, as long as their conditions hold:
    // r1: 2f+1 r3 blocks of the previous wave;
    // r2: 2f+1 r1 blocks, including our own r1 block with its QC1;
    // r3: the wave leader is revealed and 2f+1 r2 blocks.
    async fn try_advance(&mut self) -> ConsensusResult<()> {
        loop {
            let round = self.last_proposed_round + 1;
            let wave = wave_of(round);
            let ready = match phase_of(round) {
                R1 => round == 1 || self.has_quorum(round - 1),
                R2 => self.my_qc1.contains_key(&wave) && self.has_quorum(round - 1),
                _ => self.leaders.contains_key(&wave) && self.has_quorum(round - 1),
            };
            if !ready {
                return Ok(());
            }
            if self.parameters.min_block_delay > 0 {
                let deadline =
                    self.last_proposal_time + Duration::from_millis(self.parameters.min_block_delay);
                if Instant::now() < deadline {
                    self.proposal_deadline = Some(deadline);
                    return Ok(());
                }
            }
            self.propose(round).await?;
        }
    }

    // Weak references: certified blocks not reachable from the strong references.
    fn weak_references(
        &mut self,
        round: SeqNumber,
        strong: &[(PublicKey, Digest, SeqNumber)],
    ) -> Vec<(PublicKey, Digest, SeqNumber)> {
        if round < 3 {
            return Vec::new();
        }
        let max_round = round - 2;
        let mut covered = Vec::new();
        let mut candidates = Vec::new();
        for (candidate_round, author) in self
            .weak_candidates
            .iter()
            .take_while(|(candidate_round, _)| *candidate_round <= max_round)
        {
            // Certified blocks we do not have yet stay candidates.
            if let Some((digest, _)) = self.dag.get_at(*candidate_round, author) {
                covered.push((*candidate_round, *author));
                if !self.committer.is_committed(digest) {
                    candidates.push((*author, digest.clone(), *candidate_round));
                }
            }
        }
        // After this block, every candidate in our DAG is covered by it.
        for key in &covered {
            self.weak_candidates.remove(key);
        }
        if candidates.is_empty() {
            return Vec::new();
        }

        let min_round = candidates.iter().map(|(_, _, round)| *round).min().unwrap();
        let strong: Vec<Digest> = strong.iter().map(|(_, digest, _)| digest.clone()).collect();
        let committer = &self.committer;
        let reachable: HashSet<Digest> = self
            .dag
            .causal_history(&strong, min_round, |digest| committer.is_committed(digest))
            .into_iter()
            .collect();
        candidates
            .into_iter()
            .filter(|(_, digest, _)| !reachable.contains(digest))
            .collect()
    }

    async fn propose(&mut self, round: SeqNumber) -> ConsensusResult<()> {
        let wave = wave_of(round);
        let phase = phase_of(round);

        // Strong references: every block of the previous round in our view.
        let mut references: Vec<(PublicKey, Digest, SeqNumber)> = self
            .dag
            .round(round - 1)
            .map(|(digest, block)| (block.author, digest.clone(), round - 1))
            .collect();
        let weak = self.weak_references(round, &references);
        references.extend(weak);
        let qc = match phase {
            R2 => self.my_qc1[&wave].clone(),
            _ => QC::genesis(),
        };
        // From now on, we no longer vote for the r2 block of the wave leader
        // if this is our r3 block (exit vote).
        self.last_proposed_round = round;
        self.last_proposal_time = Instant::now();

        // Make the block.
        let payload = self
            .mempool_driver
            .get(self.parameters.max_payload_size)
            .await;
        let block = Block::new(
            qc,
            self.name,
            round,
            payload,
            references,
            self.signature_service.clone(),
        )
        .await;
        let digest = block.digest();

        if !block.payload.is_empty() {
            info!("Created {}", block);

            #[cfg(feature = "benchmark")]
            for x in &block.payload {
                // NOTE: This log entry is used to compute performance.
                info!("Created {} -> {:?}", block, x);
            }
        }
        debug!("Created {:?}", block);
        self.my_blocks.insert(round, digest.clone());

        // Broadcast our new block.
        let message = ConsensusMessage::Propose(block.clone());
        Synchronizer::transmit(
            message,
            &self.name,
            None,
            &self.network_filter,
            &self.committee,
            BLOCK_CHANNEL,
        )
        .await?;

        // Our own payload is already in the mempool's store.
        self.insert_blocks(digest, block).await
    }

    async fn handle_proposal(&mut self, block: Block) -> ConsensusResult<()> {
        let digest = block.digest();
        if self.is_known(&digest, &block) {
            return Ok(());
        }

        // Check the block is correctly formed.
        block.verify(&self.committee)?;

        // Let's see if we have the block's data. If we don't, the mempool
        // will get it and then make us resume processing this block.
        if !self.mempool_driver.verify(block.clone()).await? {
            debug!("Processing of {} suspended: missing payload", digest);
            return Ok(());
        }

        self.process_block(digest, block).await
    }

    fn is_known(&self, digest: &Digest, block: &Block) -> bool {
        block.height < self.dag.gc_round()
            || self.dag.contains(digest)
            || self.pending.contains_key(digest)
    }

    // Inserts the block in the DAG once all its parents are there.
    async fn process_block(&mut self, digest: Digest, block: Block) -> ConsensusResult<()> {
        if self.is_known(&digest, &block) {
            return Ok(());
        }
        self.synchronizer.received(&digest);

        let missing = self.dag.missing_parents(&block);
        if missing.is_empty() {
            return self.insert_blocks(digest, block).await;
        }
        debug!("Processing of {} suspended: {} missing parents", digest, missing.len());
        for (parent, round) in &missing {
            self.waiting
                .entry(parent.clone())
                .or_insert_with(Vec::new)
                .push(digest.clone());
            if !self.pending.contains_key(parent) {
                self.synchronizer.add_missing(parent.clone(), *round, block.author);
            }
        }
        self.pending.insert(digest, (block, missing.len()));
        Ok(())
    }

    // Inserts the block, then every pending block it unblocks.
    async fn insert_blocks(&mut self, digest: Digest, block: Block) -> ConsensusResult<()> {
        let mut ready = vec![(digest, block)];
        while let Some((digest, block)) = ready.pop() {
            self.store_block(&digest, &block).await;
            self.synchronizer.received(&digest);
            if let Err(e) = self.on_insert(&digest, &block).await {
                warn!("{}", e);
            }
            self.dag.insert(digest.clone(), block);

            if let Some(children) = self.waiting.remove(&digest) {
                for child in children {
                    if let Some((_, missing)) = self.pending.get_mut(&child) {
                        *missing -= 1;
                        if *missing == 0 {
                            let (block, _) = self.pending.remove(&child).unwrap();
                            ready.push((child, block));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn on_insert(&mut self, digest: &Digest, block: &Block) -> ConsensusResult<()> {
        // The QC1 embedded in an r2 block certifies its author's r1 block.
        if block.phase() == R2 && block.author != self.name {
            self.weak_candidates.insert((block.height - 1, block.author));
        }
        self.make_vote(digest, block).await
    }

    // We vote once for every r1 (PB) and r2 (CBC) block inserted in our DAG.
    async fn make_vote(&mut self, digest: &Digest, block: &Block) -> ConsensusResult<()> {
        let phase = block.phase();
        if phase == R3 {
            return Ok(());
        }
        // Exit vote: once our r3 block of the wave is out, we no longer vote
        // for the r2 block of the wave leader.
        let wave = block.wave();
        if phase == R2
            && self.last_proposed_round >= round_of(wave, R3)
            && self.leaders.get(&wave) == Some(&block.author)
        {
            debug!("Exit vote for leader block {} of wave {}", digest, wave);
            return Ok(());
        }

        let vote = Vote::new(digest.clone(), block, self.name, self.signature_service.clone()).await;
        debug!("Created {:?}", vote);
        if block.author == self.name {
            return self.process_vote(vote).await;
        }
        let message = ConsensusMessage::Vote(vote);
        Synchronizer::transmit(
            message,
            &self.name,
            Some(&block.author),
            &self.network_filter_fast,
            &self.committee,
            FAST_CHANNEL,
        )
        .await
    }

    async fn handle_vote(&mut self, vote: Vote) -> ConsensusResult<()> {
        // Ignore votes for blocks that are not (or no longer) ours.
        if vote.proposer != self.name || self.my_blocks.get(&vote.height) != Some(&vote.hash) {
            return Ok(());
        }

        // Ensure the vote is well formed.
        vote.verify(&self.committee)?;
        self.process_vote(vote).await
    }

    async fn process_vote(&mut self, vote: Vote) -> ConsensusResult<()> {
        // Add the new vote to our aggregator and see if we have a quorum.
        let qc = match self.aggregator.add_vote(vote)? {
            Some(qc) => qc,
            None => return Ok(()),
        };
        debug!("Assembled {:?}", qc);
        let wave = wave_of(qc.height);
        if phase_of(qc.height) == R1 {
            self.my_qc1.insert(wave, qc);
            return Ok(());
        }

        // QC2: finish the CBC by broadcasting it with our coin share.
        let coin = CoinShare::new(wave, self.name, self.signature_service.clone()).await;
        let certificate = Certificate { qc, coin };
        let message = ConsensusMessage::Certificate(certificate.clone());
        Synchronizer::transmit(
            message,
            &self.name,
            None,
            &self.network_filter_fast,
            &self.committee,
            FAST_CHANNEL,
        )
        .await?;
        self.process_certificate(certificate)
    }

    async fn handle_certificate(&mut self, certificate: Certificate) -> ConsensusResult<()> {
        if certificate.qc.height < self.dag.gc_round() {
            return Ok(());
        }
        certificate.qc.verify(&self.committee)?;
        // Coin shares are only needed until the wave leader is revealed (and
        // each verification costs a pairing).
        if !self.leaders.contains_key(&wave_of(certificate.qc.height)) {
            certificate.coin.verify(&self.committee, &self.pk_set)?;
        }
        self.process_certificate(certificate)
    }

    fn process_certificate(&mut self, certificate: Certificate) -> ConsensusResult<()> {
        debug!("Processing {:?}", certificate);
        let qc = certificate.qc;
        let wave = wave_of(qc.height);
        if qc.proposer != self.name {
            self.weak_candidates.insert((qc.height, qc.proposer));
        }
        self.qc2s.insert((wave, qc.proposer), qc.hash);

        // f+1 coin shares reveal the wave leader.
        if self.leaders.contains_key(&wave) {
            return Ok(());
        }
        if let Some(coin) = self.aggregator.add_coin_share(certificate.coin, &self.pk_set)? {
            let leader = self.leader_elector.get_leader(coin as SeqNumber);
            debug!("Leader of wave {} is {}", wave, leader);
            self.leaders.insert(wave, leader);
        }
        Ok(())
    }

    // Direct commit: the wave leader's r2 block has a QC2 (and is in our DAG);
    // its QC1 names the leader block (r1) to commit.
    async fn try_commit(&mut self) -> ConsensusResult<()> {
        loop {
            let next_wave = self.committer.last_committed_wave() + 1;
            let anchor = self.leaders.range(next_wave..).find_map(|(wave, leader)| {
                let digest = self.qc2s.get(&(*wave, *leader))?;
                Some(self.dag.get(digest)?.qc.hash.clone())
            });
            let anchor = match anchor {
                Some(anchor) => anchor,
                None => return Ok(()),
            };
            let leaders = &self.leaders;
            let sub_dags = match self
                .committer
                .commit(&self.dag, &anchor, |wave| leaders.get(&wave).cloned())
            {
                Some(sub_dags) => sub_dags,
                // Waiting for the leader of an earlier wave.
                None => return Ok(()),
            };
            for sub_dag in sub_dags {
                self.deliver(sub_dag).await;
            }
            self.cleanup();
        }
    }

    async fn deliver(&mut self, sub_dag: CommittedSubDag) {
        debug!(
            "Committing leader {} of wave {} with {} blocks",
            sub_dag.leader,
            sub_dag.wave,
            sub_dag.blocks.len()
        );
        let mut digests = Vec::new();
        for (digest, block) in sub_dag.blocks {
            self.commit_index += 1;
            // NOTE: This log entry is used to check that all nodes commit the same sequence.
            info!("Sequenced {} {} {} {}", self.commit_index, digest, block, block.author);
            if !block.payload.is_empty() {
                info!("Committed {}", block);

                #[cfg(feature = "benchmark")]
                for x in &block.payload {
                    // NOTE: This log entry is used to compute performance.
                    info!("Committed {} -> {:?}", block, x);
                }
                digests.extend(block.payload.iter().cloned());
            }
            if let Err(e) = self.commit_channel.send(block).await {
                warn!("Failed to send block through the commit channel: {}", e);
            }
        }

        // Cleanup the mempool. It drops pending payload requests of blocks up to
        // this round, so only rounds that we no longer keep are passed.
        let gc_round = self.committer.gc_round();
        self.mempool_driver
            .cleanup(digests, gc_round.saturating_sub(1))
            .await;
    }

    fn cleanup(&mut self) {
        let gc_round = self.committer.gc_round();
        if gc_round <= self.dag.gc_round() {
            return;
        }
        let gc_wave = wave_of(gc_round);
        self.dag.gc(gc_round);
        self.pending.retain(|_, (block, _)| block.height >= gc_round);
        let pending = &self.pending;
        self.waiting.retain(|_, children| {
            children.retain(|child| pending.contains_key(child));
            !children.is_empty()
        });
        self.synchronizer.cleanup(gc_round);
        self.aggregator.cleanup(gc_round, gc_wave);
        self.my_blocks.retain(|round, _| *round >= gc_round);
        self.my_qc1.retain(|wave, _| *wave >= gc_wave);
        self.qc2s.retain(|(wave, _), _| *wave >= gc_wave);
        self.leaders.retain(|wave, _| *wave >= gc_wave);
        self.weak_candidates.retain(|(round, _)| *round >= gc_round);
    }

    async fn handle_sync_request(
        &mut self,
        digests: Vec<Digest>,
        sender: PublicKey,
    ) -> ConsensusResult<()> {
        for digest in digests {
            let block = match self.dag.get(&digest) {
                Some(block) => Some(block.clone()),
                None => match self.store.read(digest.to_vec()).await? {
                    Some(bytes) => Some(bincode::deserialize(&bytes)?),
                    None => None,
                },
            };
            if let Some(block) = block {
                let message = ConsensusMessage::SyncReply(block);
                Synchronizer::transmit(
                    message,
                    &self.name,
                    Some(&sender),
                    &self.network_filter,
                    &self.committee,
                    BLOCK_CHANNEL,
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn run(&mut self) {
        let accuracy = min(self.parameters.sync_retry_delay, 1_000).max(1);
        let mut sync_timer = interval(Duration::from_millis(accuracy));

        // Propose the first block.
        if let Err(e) = self.try_advance().await {
            warn!("{}", e);
        }

        loop {
            let deadline = self.proposal_deadline;
            let result = tokio::select! {
                Some(message) = self.core_channel.recv() => {
                    match message {
                        ConsensusMessage::Propose(block) => self.handle_proposal(block).await,
                        ConsensusMessage::SyncReply(block) => self.handle_proposal(block).await,
                        ConsensusMessage::LoopBack(block) => self.process_block(block.digest(), block).await,
                        ConsensusMessage::SyncRequest(digests, sender) => self.handle_sync_request(digests, sender).await,
                        message => {
                            warn!("Unexpected message on the block channel: {:?}", message);
                            Ok(())
                        }
                    }
                },
                Some(message) = self.fast_channel.recv() => {
                    match message {
                        ConsensusMessage::Vote(vote) => self.handle_vote(vote).await,
                        ConsensusMessage::Certificate(certificate) => self.handle_certificate(certificate).await,
                        message => {
                            warn!("Unexpected message on the fast channel: {:?}", message);
                            Ok(())
                        }
                    }
                },
                _ = sync_timer.tick() => self.synchronizer.request_expired().await,
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.proposal_deadline = None;
                    Ok(())
                },
            };
            Self::log_error(result);

            // Proposals are on the critical path, so advance before committing.
            Self::log_error(self.try_advance().await);
            Self::log_error(self.try_commit().await);
        }
    }

    fn log_error(result: ConsensusResult<()>) {
        match result {
            Ok(()) => (),
            Err(ConsensusError::StoreError(e)) => error!("{}", e),
            Err(ConsensusError::SerializationError(e)) => error!("Store corrupted. {}", e),
            Err(e) => warn!("{}", e),
        }
    }
}
