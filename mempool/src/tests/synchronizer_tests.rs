use super::*;
use crate::common::{block, committee, keys};
use rand::rngs::StdRng;
use rand::RngCore as _;
use rand::SeedableRng as _;
use std::fs;

#[tokio::test]
async fn verify_empty() {
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_network, _rx_network) = channel(1);
    let store_path = ".db_test_verify_empty";
    let _ = fs::remove_dir_all(store_path);
    let store = Store::new(store_path).unwrap();
    let (name, _) = keys().pop().unwrap();
    let mut synchronizer = Synchronizer::new(
        tx_consensus,
        store,
        name,
        committee(),
        tx_network,
        /* sync_retry_delay */ 10_000,
    );
    let result = synchronizer.verify_payload(block()).await;
    assert!(result.is_ok());
    assert!(result.unwrap());
}

#[tokio::test]
async fn verify_wait() {
    let (tx_consensus, mut rx_consensus) = channel(1);
    let (tx_network, mut rx_network) = channel(1);
    let store_path = ".db_test_verify_wait";
    let _ = fs::remove_dir_all(store_path);
    let mut store = Store::new(store_path).unwrap();
    let (name, _) = keys().pop().unwrap();
    let mut synchronizer = Synchronizer::new(
        tx_consensus,
        store.clone(),
        name,
        committee(),
        tx_network,
        /* sync_retry_delay */ 10_000,
    );

    // Make a block with two payloads.
    let mut rng = StdRng::from_seed([0; 32]);
    let mut payload_1 = [0u8; 32];
    rng.fill_bytes(&mut payload_1);
    let mut payload_2 = [0u8; 32];
    rng.fill_bytes(&mut payload_2);
    let payload = vec![Digest(payload_1), Digest(payload_2)];
    let block = Block {
        payload: payload.clone(),
        ..block()
    };
    let digest = block.digest();

    // Ensure the synchronizer replies with WAIT.
    let result = synchronizer.verify_payload(block).await;
    assert!(result.is_ok());
    assert!(!result.unwrap());

    // Ensure the synchronizer waits for the payloads rather than requesting them.
    let request = tokio::time::timeout(Duration::from_millis(500), rx_network.recv()).await;
    assert!(request.is_err());

    // Add the payload to the store and ensure the driver trigger re-processing.
    let _ = store.write(payload_1.to_vec(), Vec::new()).await;
    let _ = store.write(payload_2.to_vec(), Vec::new()).await;
    match rx_consensus.recv().await {
        Some(ConsensusMessage::LoopBack(b)) => assert_eq!(b.digest(), digest),
        _ => assert!(false),
    }
}

#[tokio::test]
async fn verify_retry() {
    let (tx_consensus, _rx_consensus) = channel(1);
    let (tx_network, mut rx_network) = channel(1);
    let store_path = ".db_test_verify_retry";
    let _ = fs::remove_dir_all(store_path);
    let store = Store::new(store_path).unwrap();
    let (name, _) = keys().pop().unwrap();
    let mut synchronizer = Synchronizer::new(
        tx_consensus,
        store,
        name,
        committee(),
        tx_network,
        /* sync_retry_delay */ 0,
    );

    // Make a block with a payload that never arrives.
    let mut rng = StdRng::from_seed([1; 32]);
    let mut payload_1 = [0u8; 32];
    rng.fill_bytes(&mut payload_1);
    let block = Block {
        payload: vec![Digest(payload_1)],
        ..block()
    };
    let result = synchronizer.verify_payload(block).await;
    assert!(result.is_ok());
    assert!(!result.unwrap());

    // Once `sync_retry_delay` expires (checked every 5s), the payload is
    // requested from all other nodes.
    match tokio::time::timeout(Duration::from_secs(10), rx_network.recv()).await {
        Ok(Some(NetMessage(bytes, mut recipients))) => {
            match bincode::deserialize(&bytes).unwrap() {
                MempoolMessage::PayloadRequest(p, s) => {
                    assert_eq!(p, vec![Digest(payload_1)]);
                    assert_eq!(s, name);
                }
                _ => assert!(false),
            }
            let mut expected = committee().broadcast_addresses(&name);
            recipients.sort();
            expected.sort();
            assert_eq!(recipients, expected);
        }
        _ => assert!(false),
    }
}
