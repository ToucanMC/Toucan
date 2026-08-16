use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use bytes::Bytes;
use toucan_world::{Chunk, ChunkEncodingKey, ChunkPosition};

pub(crate) struct EncodedChunkCache {
    capacity: usize,
    state: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<ChunkPosition, CacheEntry>,
    access_clock: u64,
}

struct CacheEntry {
    key: ChunkEncodingKey,
    owner: Weak<Chunk>,
    payload: Bytes,
    last_access: u64,
}

impl EncodedChunkCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(CacheState::default()),
        }
    }

    pub(crate) fn get(&self, key: ChunkEncodingKey, owner: &Arc<Chunk>) -> Option<Bytes> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let valid = state.entries.get(&key.position).is_some_and(|entry| {
            entry.key == key
                && entry
                    .owner
                    .upgrade()
                    .is_some_and(|cached| Arc::ptr_eq(&cached, owner))
        });
        if !valid {
            state.entries.remove(&key.position);
            return None;
        }
        state.access_clock = state.access_clock.wrapping_add(1);
        let access_clock = state.access_clock;
        let entry = state.entries.get_mut(&key.position)?;
        entry.last_access = access_clock;
        Some(entry.payload.clone())
    }

    pub(crate) fn insert(&self, key: ChunkEncodingKey, owner: &Arc<Chunk>, payload: Bytes) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.access_clock = state.access_clock.wrapping_add(1);
        let last_access = state.access_clock;
        state.entries.insert(
            key.position,
            CacheEntry {
                key,
                owner: Arc::downgrade(owner),
                payload,
                last_access,
            },
        );
        while state.entries.len() > self.capacity {
            let Some(position) = state
                .entries
                .iter()
                .min_by_key(|(position, entry)| (entry.last_access, position.x, position.z))
                .map(|(position, _)| *position)
            else {
                break;
            };
            state.entries.remove(&position);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use toucan_world::{BlockStateId, Chunk, ChunkEncodingKey, ChunkPosition};

    use super::EncodedChunkCache;

    fn key(position: ChunkPosition, revision: u64, protocol_version: i32) -> ChunkEncodingKey {
        ChunkEncodingKey {
            position,
            revision,
            protocol_version,
        }
    }

    #[test]
    fn revisions_protocols_and_chunk_instances_do_not_reuse_stale_payloads() {
        let position = ChunkPosition { x: 2, z: -3 };
        let first = Arc::new(Chunk::empty(position));
        let replacement = Arc::new(Chunk::empty(position));
        let cache = EncodedChunkCache::new(2);
        let initial = key(position, first.revision(), 775);
        cache.insert(initial, &first, Bytes::from_static(b"initial"));
        assert_eq!(
            cache.get(initial, &first),
            Some(Bytes::from_static(b"initial"))
        );
        assert_eq!(cache.get(key(position, 0, 774), &first), None);

        cache.insert(initial, &first, Bytes::from_static(b"initial"));
        assert!(first.set_block(0, 64, 0, BlockStateId::STONE));
        assert_eq!(
            cache.get(key(position, first.revision(), 775), &first),
            None
        );

        cache.insert(initial, &first, Bytes::from_static(b"initial"));
        assert_eq!(cache.get(initial, &replacement), None);
    }

    #[test]
    fn capacity_uses_deterministic_lru_eviction() {
        let cache = EncodedChunkCache::new(2);
        let first = Arc::new(Chunk::empty(ChunkPosition { x: 0, z: 0 }));
        let second = Arc::new(Chunk::empty(ChunkPosition { x: 1, z: 0 }));
        let third = Arc::new(Chunk::empty(ChunkPosition { x: 2, z: 0 }));
        let first_key = key(first.position(), 0, 775);
        let second_key = key(second.position(), 0, 775);
        let third_key = key(third.position(), 0, 775);
        cache.insert(first_key, &first, Bytes::from_static(b"first"));
        cache.insert(second_key, &second, Bytes::from_static(b"second"));
        assert!(cache.get(first_key, &first).is_some());
        cache.insert(third_key, &third, Bytes::from_static(b"third"));

        assert_eq!(cache.get(second_key, &second), None);
        assert!(cache.get(first_key, &first).is_some());
        assert!(cache.get(third_key, &third).is_some());
    }
}
