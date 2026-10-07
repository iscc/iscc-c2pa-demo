//! A process-wide memo of slow results keyed by the BLAKE3 hash of their input: the Semantic-Codes
//! by their model input, OCR text by the rendered page. A file, its signed copy and that copy
//! reopened usually give the same input, so each is computed once per session.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// Results by the hash of their input, up to a capacity after which the memo starts over.
pub struct Memo<V> {
    map: Mutex<Option<HashMap<[u8; 32], V>>>,
    capacity: usize,
}

impl<V: Clone> Memo<V> {
    /// An empty memo that keeps at most `capacity` results.
    pub const fn new(capacity: usize) -> Self {
        Memo {
            map: Mutex::new(None),
            capacity,
        }
    }

    /// The result kept for the input hashed to `key`.
    pub fn recall(&self, key: &[u8; 32]) -> Option<V> {
        self.lock().as_ref()?.get(key).cloned()
    }

    /// Keep `value` for the input hashed to `key`; a full memo starts over.
    pub fn keep(&self, key: [u8; 32], value: V) {
        let mut memo = self.lock();
        let map = memo.get_or_insert_with(HashMap::new);
        if map.len() >= self.capacity {
            map.clear();
        }
        map.insert(key, value);
    }

    /// The map, locked; a panic elsewhere while it was locked leaves it usable.
    fn lock(&self) -> MutexGuard<'_, Option<HashMap<[u8; 32], V>>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: usize) -> [u8; 32] {
        *blake3::hash(format!("memo test {i}").as_bytes()).as_bytes()
    }

    #[test]
    fn memo_remembers_and_starts_over_when_full() {
        let memo = Memo::new(3);
        assert_eq!(memo.recall(&key(0)), None);
        memo.keep(key(0), "first");
        assert_eq!(memo.recall(&key(0)), Some("first"));
        memo.keep(key(0), "again");
        assert_eq!(memo.recall(&key(0)), Some("again"), "replaced");
        memo.keep(key(1), "second");
        memo.keep(key(2), "third");
        assert_eq!(memo.recall(&key(0)), Some("again"));
        memo.keep(key(3), "fourth");
        assert_eq!(memo.recall(&key(0)), None, "the memo started over");
        assert_eq!(memo.recall(&key(3)), Some("fourth"));
    }
}
