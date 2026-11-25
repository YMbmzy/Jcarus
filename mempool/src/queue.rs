use crypto::Digest;
use std::collections::{BTreeMap, HashMap};

#[cfg(test)]
#[path = "tests/queue_tests.rs"]
pub mod queue_tests;

/// A set of payload digests that hands them out oldest first. With a plain
/// `HashSet` the digests put in a block are arbitrary, so a payload that
/// nobody else includes (e.g. one from a lagging node) can keep losing its
/// slot to fresher ones.
#[derive(Default)]
pub struct DigestQueue {
    next: u64,
    order: BTreeMap<u64, Digest>,
    index: HashMap<Digest, u64>,
}

impl DigestQueue {
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Inserting a digest already in the queue keeps its original position.
    pub fn insert(&mut self, digest: Digest) {
        if self.index.contains_key(&digest) {
            return;
        }
        self.order.insert(self.next, digest.clone());
        self.index.insert(digest, self.next);
        self.next += 1;
    }

    pub fn remove(&mut self, digest: &Digest) {
        if let Some(position) = self.index.remove(digest) {
            self.order.remove(&position);
        }
    }

    /// Removes and returns up to `n` of the oldest digests.
    pub fn take(&mut self, n: usize) -> Vec<Digest> {
        let mut digests = Vec::with_capacity(n.min(self.len()));
        while digests.len() < n {
            let position = match self.order.keys().next() {
                Some(position) => *position,
                None => break,
            };
            let digest = self.order.remove(&position).unwrap();
            self.index.remove(&digest);
            digests.push(digest);
        }
        digests
    }
}
