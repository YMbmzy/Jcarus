use super::*;
use crate::common::{committee, keys, NODES};
use tokio::sync::mpsc::channel;

#[tokio::test]
async fn requests_after_the_delay() {
    let (tx_network, mut rx_network) = channel(10);
    let (name, _) = keys().pop().unwrap();
    let (origin, _) = keys()[0];
    let mut synchronizer = Synchronizer::new(name, committee(), tx_network, /* sync_retry_delay */ 0);
    let digest = Digest([1u8; 32]);
    synchronizer.add_missing(digest.clone(), 5, origin);

    // The first request goes to the node referencing the block.
    synchronizer.request_expired().await.unwrap();
    let (message, addresses) = rx_network.recv().await.unwrap();
    match message {
        ConsensusMessage::SyncRequest(digests, sender) => {
            assert_eq!(digests, vec![digest.clone()]);
            assert_eq!(sender, name);
        }
        _ => assert!(false),
    }
    assert_eq!(addresses, vec![committee().address(&origin).unwrap()]);

    // Retries are broadcast.
    synchronizer.request_expired().await.unwrap();
    let (_, addresses) = rx_network.recv().await.unwrap();
    assert_eq!(addresses.len(), NODES - 1);

    // Nothing is requested once the block is received.
    synchronizer.received(&digest);
    synchronizer.request_expired().await.unwrap();
    assert!(rx_network.try_recv().is_err());
}

#[tokio::test]
async fn no_request_before_the_delay() {
    let (tx_network, mut rx_network) = channel(10);
    let (name, _) = keys().pop().unwrap();
    let (origin, _) = keys()[0];
    let mut synchronizer = Synchronizer::new(name, committee(), tx_network, /* sync_retry_delay */ 10_000);
    synchronizer.add_missing(Digest([1u8; 32]), 5, origin);
    synchronizer.request_expired().await.unwrap();
    assert!(rx_network.try_recv().is_err());

    // Requests below the GC round are dropped.
    synchronizer.cleanup(6);
    assert!(synchronizer.missing.is_empty());
}
