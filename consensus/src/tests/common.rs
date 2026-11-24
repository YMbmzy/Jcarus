use crate::config::Committee;
use crate::core::SeqNumber;
use crate::mempool::{ConsensusMempoolMessage, PayloadStatus};
use crate::messages::{phase_of, Block, Vote, QC, R2};
use crypto::Hash as _;
use crypto::{generate_keypair, Digest, PublicKey, SecretKey, SecretShare, Signature};
use rand::rngs::StdRng;
use rand::RngCore as _;
use rand::SeedableRng as _;
use threshold_crypto::serde_impl::SerdeSecret;
use threshold_crypto::SecretKeySet;
use tokio::sync::mpsc::Receiver;

pub const NODES: usize = 4;

// Fixture.
pub fn keys() -> Vec<(PublicKey, SecretKey)> {
    let mut rng = StdRng::from_seed([0; 32]);
    (0..NODES).map(|_| generate_keypair(&mut rng)).collect()
}

// Fixture. Node i holds the threshold key share i.
pub fn committee() -> Committee {
    Committee::new(
        keys()
            .into_iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let address = format!("0.0.0.0:{}", i).parse().unwrap();
                let smvba_address = format!("0.0.0.0:{}", 100 + i).parse().unwrap();
                let stake = 1;
                (name, i, stake, address, smvba_address)
            })
            .collect(),
        /* epoch */ 1,
    )
}

// Fixture. Threshold key shares (f+1 of them reveal the coin), indexed like `keys()`.
pub fn tss_keys() -> Vec<SecretShare> {
    let mut rng = StdRng::from_seed([1; 32]);
    let sk_set = SecretKeySet::random((NODES - 1) / 3, &mut rng);
    let pk_set = sk_set.public_keys();
    (0..NODES)
        .map(|i| {
            SecretShare::new(
                i,
                pk_set.public_key_share(i),
                SerdeSecret(sk_set.secret_key_share(i)),
                pk_set.clone(),
            )
        })
        .collect()
}

impl Committee {
    pub fn increment_base_port(&mut self, base_port: u16) {
        for authority in self.authorities.values_mut() {
            let port = authority.address.port();
            let port_s = authority.smvba_address.port();
            authority.address.set_port(base_port + port);
            authority.smvba_address.set_port(base_port + port_s);
        }
    }
}

impl Block {
    pub fn new_from_key(
        qc: QC,
        author: PublicKey,
        height: SeqNumber,
        payload: Vec<Digest>,
        references: Vec<(PublicKey, Digest, SeqNumber)>,
        secret: &SecretKey,
    ) -> Self {
        let block = Block {
            qc,
            author,
            height,
            epoch: crate::messages::wave_of(height),
            total_epoch: 0,
            payload,
            references,
            signature: Signature::default(),
        };
        let signature = Signature::new(&block.digest(), secret);
        Self { signature, ..block }
    }
}

impl PartialEq for Block {
    fn eq(&self, other: &Self) -> bool {
        self.digest() == other.digest()
    }
}

impl Vote {
    pub fn new_from_key(
        hash: Digest,
        height: SeqNumber,
        proposer: PublicKey,
        author: PublicKey,
        secret: &SecretKey,
    ) -> Self {
        let vote = Self {
            hash,
            height,
            epoch: crate::messages::wave_of(height),
            proposer,
            author,
            signature: Signature::default(),
        };
        let signature = Signature::new(&vote.digest(), &secret);
        Self { signature, ..vote }
    }
}

impl PartialEq for Vote {
    fn eq(&self, other: &Self) -> bool {
        self.digest() == other.digest()
    }
}

// Fixture.
pub fn block() -> Block {
    let (public_key, secret_key) = keys().pop().unwrap();
    Block::new_from_key(QC::genesis(), public_key, 1, Vec::new(), Vec::new(), &secret_key)
}

// Fixture.
pub fn vote() -> Vote {
    let (public_key, secret_key) = keys().pop().unwrap();
    Vote::new_from_key(block().digest(), 1, block().author, public_key, &secret_key)
}

// Fixture. A QC made of the votes of the first three nodes for `hash`.
pub fn qc_for(hash: Digest, height: SeqNumber, proposer: PublicKey) -> QC {
    let qc = QC {
        hash,
        height,
        epoch: crate::messages::wave_of(height),
        proposer,
        votes: Vec::new(),
    };
    let digest = qc.digest();
    let votes: Vec<_> = keys()
        .into_iter()
        .take(3)
        .map(|(public_key, secret_key)| (public_key, Signature::new(&digest, &secret_key)))
        .collect();
    QC { votes, ..qc }
}

// Fixture.
pub fn qc() -> QC {
    let (public_key, _) = keys().pop().unwrap();
    qc_for(Digest::default(), 1, public_key)
}

// Builds unsigned DAG blocks for the DAG and committer tests. `parents` are the
// strong references (blocks of the previous round); r2 blocks also reference
// their author's r1 block through `qc`.
pub fn dag_block(
    author: PublicKey,
    height: SeqNumber,
    parents: &[(Digest, &Block)],
    weak: &[(Digest, &Block)],
    own_r1: Option<Digest>,
) -> (Digest, Block) {
    let mut references: Vec<_> = parents
        .iter()
        .map(|(digest, block)| (block.author, digest.clone(), block.height))
        .collect();
    references.extend(
        weak.iter()
            .map(|(digest, block)| (block.author, digest.clone(), block.height)),
    );
    let qc = match (phase_of(height), own_r1) {
        (R2, Some(hash)) => QC {
            hash,
            height: height - 1,
            epoch: crate::messages::wave_of(height),
            proposer: author,
            votes: Vec::new(),
        },
        _ => QC::genesis(),
    };
    let block = Block {
        qc,
        author,
        height,
        epoch: crate::messages::wave_of(height),
        total_epoch: 0,
        payload: Vec::new(),
        references,
        signature: Signature::default(),
    };
    (block.digest(), block)
}

// Fixture
pub struct MockMempool;

impl MockMempool {
    pub fn run(mut consensus_mempool_channel: Receiver<ConsensusMempoolMessage>) {
        tokio::spawn(async move {
            let mut rng = StdRng::from_seed([0; 32]);
            while let Some(message) = consensus_mempool_channel.recv().await {
                match message {
                    ConsensusMempoolMessage::Get(_max, sender) => {
                        let mut payload = [0u8; 32];
                        rng.fill_bytes(&mut payload);
                        sender.send(vec![Digest(payload)]).unwrap();
                    }
                    ConsensusMempoolMessage::Verify(_block, sender) => {
                        sender.send(PayloadStatus::Accept).unwrap()
                    }
                    ConsensusMempoolMessage::Cleanup(_digests, _round) => (),
                }
            }
        });
    }
}
