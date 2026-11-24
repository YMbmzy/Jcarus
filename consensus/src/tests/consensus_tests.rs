use super::*;
use crate::common::{committee, keys, tss_keys, MockMempool};
use crate::config::Parameters;
use crypto::{Digest, Hash as _, SecretKey, SecretShare};
use futures::future::try_join_all;
use std::fs;
use tokio::sync::mpsc::channel;
use tokio::task::JoinHandle;

const COMMITTED: usize = 40;

// Runs the given nodes and returns the digests of the first committed blocks of each.
fn spawn_nodes(
    nodes: Vec<((PublicKey, SecretKey), SecretShare)>,
    committee: Committee,
    store_path: &str,
) -> Vec<JoinHandle<Vec<Digest>>> {
    nodes
        .into_iter()
        .enumerate()
        .map(|(i, ((name, secret), tss_key))| {
            let committee = committee.clone();
            let parameters = Parameters {
                min_block_delay: 0,
                ..Parameters::default()
            };
            let pk_set = tss_key.pkset.clone();
            let signature_service = SignatureService::new(secret, Some(tss_key.secret.into_inner()));
            let store_path = format!("{}_{}", store_path, i);
            let _ = fs::remove_dir_all(&store_path);
            let store = Store::new(&store_path).unwrap();
            let (tx_consensus, rx_consensus) = channel(100);
            let (tx_smvba, rx_smvba) = channel(100);
            let (tx_consensus_mempool, rx_consensus_mempool) = channel(100);
            MockMempool::run(rx_consensus_mempool);
            let (tx_commit, mut rx_commit) = channel(100);
            tokio::spawn(async move {
                Consensus::run(
                    name,
                    committee,
                    parameters,
                    store,
                    signature_service,
                    pk_set,
                    tx_consensus,
                    rx_consensus,
                    tx_smvba,
                    rx_smvba,
                    tx_consensus_mempool,
                    tx_commit,
                )
                .await
                .unwrap();

                let mut committed = Vec::new();
                while committed.len() < COMMITTED {
                    let block: Block = rx_commit.recv().await.unwrap();
                    committed.push(block.digest());
                }
                committed
            })
        })
        .collect()
}

#[tokio::test]
async fn end_to_end() {
    let mut committee = committee();
    committee.increment_base_port(7100);
    let nodes = keys().into_iter().zip(tss_keys()).collect();
    let handles = spawn_nodes(nodes, committee, ".db_test_end_to_end");

    // All nodes commit the same sequence.
    let sequences = try_join_all(handles).await.unwrap();
    assert!(sequences.windows(2).all(|w| w[0] == w[1]));
}

#[tokio::test]
async fn end_to_end_with_crash() {
    let mut committee = committee();
    committee.increment_base_port(7300);
    // The last node never starts.
    let nodes = keys().into_iter().zip(tss_keys()).take(3).collect();
    let handles = spawn_nodes(nodes, committee, ".db_test_end_to_end_with_crash");

    let sequences = try_join_all(handles).await.unwrap();
    assert!(sequences.windows(2).all(|w| w[0] == w[1]));
}
