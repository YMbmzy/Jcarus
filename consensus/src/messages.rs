use crate::config::Committee;
use crate::core::SeqNumber;
use crate::error::{ConsensusError, ConsensusResult};
use crypto::{Digest, Hash, PublicKey, Signature, SignatureService};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::convert::TryInto;
use std::fmt;
use threshold_crypto::{PublicKeySet, SignatureShare};

#[cfg(test)]
#[path = "tests/messages_tests.rs"]
pub mod messages_tests;

// Each wave consists of three rounds: r1 (PB), r2 (CBC) and r3 (BBC).
pub const ROUNDS_PER_WAVE: SeqNumber = 3;
pub const R1: u8 = 1;
pub const R2: u8 = 2;
pub const R3: u8 = 3;

// Global rounds start at 1: round = 3 * (wave - 1) + phase.
pub fn wave_of(round: SeqNumber) -> SeqNumber {
    (round + ROUNDS_PER_WAVE - 1) / ROUNDS_PER_WAVE
}

pub fn phase_of(round: SeqNumber) -> u8 {
    ((round - 1) % ROUNDS_PER_WAVE + 1) as u8
}

pub fn round_of(wave: SeqNumber, phase: u8) -> SeqNumber {
    ROUNDS_PER_WAVE * (wave - 1) + phase as SeqNumber
}

// The field layout is shared with the mempool crate, so the names are kept:
// `height` is the global round, `epoch` is the wave and `total_epoch` is unused.
// `qc` is the QC1 of the author's own r1 block (r2 blocks only, genesis otherwise).
// `references` holds (author, digest, round): strong references point to round
// `height - 1`, weak references to rounds `<= height - 2`.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Block {
    pub qc: QC,
    pub author: PublicKey,
    pub height: SeqNumber,
    pub epoch: SeqNumber,
    pub total_epoch: SeqNumber,
    pub payload: Vec<Digest>,
    pub references: Vec<(PublicKey, Digest, SeqNumber)>,
    pub signature: Signature,
}

impl Block {
    pub async fn new(
        qc: QC,
        author: PublicKey,
        height: SeqNumber,
        payload: Vec<Digest>,
        references: Vec<(PublicKey, Digest, SeqNumber)>,
        mut signature_service: SignatureService,
    ) -> Self {
        let block = Self {
            qc,
            author,
            height,
            epoch: wave_of(height),
            total_epoch: 0,
            payload,
            references,
            signature: Signature::default(),
        };

        let signature = signature_service.request_signature(block.digest()).await;
        Self { signature, ..block }
    }

    pub fn genesis() -> Self {
        Block::default()
    }

    pub fn wave(&self) -> SeqNumber {
        wave_of(self.height)
    }

    pub fn phase(&self) -> u8 {
        phase_of(self.height)
    }

    pub fn strong_references(&self) -> impl Iterator<Item = &Digest> {
        let parent_round = self.height.saturating_sub(1);
        self.references
            .iter()
            .filter(move |(_, _, round)| *round == parent_round)
            .map(|(_, digest, _)| digest)
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        let voting_rights = committee.stake(&self.author);
        ensure!(
            voting_rights > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check the signature.
        self.signature.verify(&self.digest(), &self.author)?;

        // Check the embedded QC1.
        if self.phase() == R2 {
            self.qc.verify(committee)?;
        }

        Ok(())
    }
}

impl Hash for Block {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(self.author.0);
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.epoch.to_le_bytes());
        hasher.update(self.total_epoch.to_le_bytes());
        for x in &self.payload {
            hasher.update(&x.0);
        }
        for x in &self.references {
            hasher.update(&x.0);
            hasher.update(&x.1.0);
            hasher.update(x.2.to_le_bytes());
        }
        hasher.update(&self.qc.hash.0);
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "{}: B(author {}, round {}, wave {}, qc {:?}, payload_len {}, references_len {})",
            self.digest(),
            self.author,
            self.height,
            self.epoch,
            self.qc,
            self.payload.iter().map(|x| x.size()).sum::<usize>(),
            self.references.len(),
        )
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "B{}", self.height)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Vote {
    pub hash: Digest,
    pub epoch: SeqNumber,
    pub height: SeqNumber,
    pub author: PublicKey, // vote author
    pub proposer: PublicKey, // block author
    pub signature: Signature,
}

impl Vote {
    pub async fn new(
        hash: Digest,
        block: &Block,
        author: PublicKey,
        mut signature_service: SignatureService,
    ) -> Self {
        let vote = Self {
            hash,
            epoch: block.epoch,
            height: block.height,
            author,
            proposer: block.author,
            signature: Signature::default(),
        };
        let signature = signature_service.request_signature(vote.digest()).await;
        Self { signature, ..vote }
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check the signature.
        self.signature.verify(&self.digest(), &self.author)?;
        Ok(())
    }
}

impl Hash for Vote {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(&self.hash.0);
        hasher.update(self.epoch.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.proposer.0);
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Vote {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "Vote(blockhash {}, proposer {}, round {}, wave {}, voter {})",
            self.hash, self.proposer, self.height, self.epoch, self.author
        )
    }
}

impl fmt::Display for Vote {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "Vote hash {}", self.hash)
    }
}

// QC1 certifies an r1 block (PB), QC2 certifies an r2 block (CBC).
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct QC {
    pub hash: Digest,
    pub epoch: SeqNumber,
    pub height: SeqNumber,
    pub proposer: PublicKey,
    pub votes: Vec<(PublicKey, Signature)>,
}

impl QC {
    pub fn genesis() -> Self {
        QC::default()
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the QC has a quorum.
        let mut weight = 0;
        let mut used = HashSet::new();
        for (name, _) in self.votes.iter() {
            ensure!(
                !used.contains(name),
                ConsensusError::AuthorityReuseinQC(*name)
            );
            let voting_rights = committee.stake(name);
            ensure!(voting_rights > 0, ConsensusError::UnknownAuthority(*name));
            used.insert(*name);
            weight += voting_rights;
        }
        ensure!(
            weight >= committee.quorum_threshold(),
            ConsensusError::QCRequiresQuorum
        );

        // Check the signatures.
        Signature::verify_batch(&self.digest(), &self.votes).map_err(ConsensusError::from)
    }
}

impl Hash for QC {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(&self.hash);
        hasher.update(self.epoch.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.proposer.0);
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for QC {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "QC(hash {}, wave {}, round {})",
            self.hash, self.epoch, self.height
        )
    }
}

impl PartialEq for QC {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.height == other.height
    }
}

// Threshold coin share of a wave; f+1 shares reveal the wave leader.
#[derive(Clone, Serialize, Deserialize)]
pub struct CoinShare {
    pub wave: SeqNumber,
    pub author: PublicKey,
    pub share: SignatureShare,
}

impl CoinShare {
    pub async fn new(
        wave: SeqNumber,
        author: PublicKey,
        mut signature_service: SignatureService,
    ) -> Self {
        let share = signature_service
            .request_tss_signature(Self::coin_digest(wave))
            .await
            .expect("Missing threshold signing key");
        Self {
            wave,
            author,
            share,
        }
    }

    pub fn coin_digest(wave: SeqNumber) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(b"coin");
        hasher.update(wave.to_le_bytes());
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }

    pub fn verify(&self, committee: &Committee, pk_set: &PublicKeySet) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );
        let tss_pk = pk_set.public_key_share(committee.id(self.author));
        // Check the signature.
        ensure!(
            tss_pk.verify(&self.share, &Self::coin_digest(self.wave)),
            ConsensusError::InvalidThresholdSignature(self.author)
        );
        Ok(())
    }
}

impl fmt::Debug for CoinShare {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "CoinShare(author {}, wave {})", self.author, self.wave)
    }
}

// The last step of the r2 CBC: the QC2 of an r2 block with its author's coin share.
#[derive(Clone, Serialize, Deserialize)]
pub struct Certificate {
    pub qc: QC,
    pub coin: CoinShare,
}

impl fmt::Debug for Certificate {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "Certificate({:?}, {:?})", self.qc, self.coin)
    }
}
