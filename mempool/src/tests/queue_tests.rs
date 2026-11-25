use super::*;

fn digest(x: u8) -> Digest {
    Digest([x; 32])
}

#[test]
fn take_oldest_first() {
    let mut queue = DigestQueue::default();
    for x in [3, 1, 2] {
        queue.insert(digest(x));
    }
    assert_eq!(queue.take(2), vec![digest(3), digest(1)]);
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.take(5), vec![digest(2)]);
    assert!(queue.is_empty());
}

#[test]
fn reinsert_keeps_position() {
    let mut queue = DigestQueue::default();
    queue.insert(digest(1));
    queue.insert(digest(2));
    queue.insert(digest(1));
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.take(2), vec![digest(1), digest(2)]);
}

#[test]
fn remove_skips_digest() {
    let mut queue = DigestQueue::default();
    for x in [1, 2, 3] {
        queue.insert(digest(x));
    }
    queue.remove(&digest(1));
    queue.remove(&digest(9));
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.take(1), vec![digest(2)]);
}
