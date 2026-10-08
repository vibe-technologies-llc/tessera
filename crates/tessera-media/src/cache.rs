use std::{
    collections::{HashMap, VecDeque},
    ops::Range,
    sync::{
        Arc, LazyLock, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::VideoFrame;

pub const FRAME_CACHE_BUDGET_BYTES: usize = 1024 * 1024 * 1024;

static SHARED_POOL: LazyLock<Arc<FramePool>> =
    LazyLock::new(|| FramePool::new(FRAME_CACHE_BUDGET_BYTES));
static NEXT_OWNER: AtomicU64 = AtomicU64::new(0);

pub struct FramePool {
    state: Mutex<PoolState>,
}

struct PoolState {
    capacity_bytes: usize,
    used_bytes: usize,
    tick: u64,
    owners: HashMap<u64, Owned>,
}

#[derive(Default)]
struct Owned {
    limit_bytes: usize,
    used_bytes: usize,
    entries: VecDeque<Entry>,
}

struct Entry {
    span: Range<i64>,
    frame: Arc<VideoFrame>,
    last_used: u64,
}

pub struct FrameCache {
    pool: Arc<FramePool>,
    owner: u64,
}

impl FramePool {
    pub fn new(capacity_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PoolState {
                capacity_bytes,
                used_bytes: 0,
                tick: 0,
                owners: HashMap::new(),
            }),
        })
    }

    pub fn shared() -> Arc<Self> {
        SHARED_POOL.clone()
    }

    #[cfg(test)]
    pub fn used_bytes(&self) -> usize {
        self.lock().used_bytes
    }

    fn lock(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PoolState {
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn evict_oldest_of(&mut self, owner: u64) -> bool {
        let Some(evicted) = self
            .owners
            .get_mut(&owner)
            .and_then(|owned| owned.pop_oldest())
        else {
            return false;
        };
        self.used_bytes -= evicted;
        true
    }

    fn evict_oldest(&mut self) -> bool {
        let oldest = self
            .owners
            .iter()
            .filter_map(|(&owner, owned)| Some((owned.entries.front()?.last_used, owner)))
            .min();
        oldest.is_some_and(|(_, owner)| self.evict_oldest_of(owner))
    }
}

impl Owned {
    fn pop_oldest(&mut self) -> Option<usize> {
        let evicted = self.entries.pop_front()?.frame.bgra.len();
        self.used_bytes -= evicted;
        Some(evicted)
    }
}

impl FrameCache {
    pub fn new(pool: &Arc<FramePool>, limit_bytes: usize) -> Self {
        let owner = NEXT_OWNER.fetch_add(1, Ordering::Relaxed);
        pool.lock().owners.insert(
            owner,
            Owned {
                limit_bytes,
                ..Owned::default()
            },
        );
        Self {
            pool: pool.clone(),
            owner,
        }
    }

    pub fn get(&mut self, ts: i64) -> Option<Arc<VideoFrame>> {
        let mut pool = self.pool.lock();
        let tick = pool.next_tick();
        let owned = pool.owners.get_mut(&self.owner)?;
        let index = owned
            .entries
            .iter()
            .position(|entry| entry.span.contains(&ts))?;
        let mut entry = owned.entries.remove(index)?;
        entry.last_used = tick;
        let frame = entry.frame.clone();
        owned.entries.push_back(entry);
        Some(frame)
    }

    pub fn contains(&self, ts: i64) -> bool {
        self.pool
            .lock()
            .owners
            .get(&self.owner)
            .is_some_and(|owned| owned.entries.iter().any(|entry| entry.span.contains(&ts)))
    }

    pub fn insert(&mut self, span: Range<i64>, frame: Arc<VideoFrame>) {
        let size = frame.bgra.len();
        let mut pool = self.pool.lock();
        let capacity = pool.capacity_bytes;
        let tick = pool.next_tick();
        let Some(owned) = pool.owners.get_mut(&self.owner) else {
            return;
        };
        if size > owned.limit_bytes || size > capacity || span.is_empty() {
            return;
        }
        let mut released = 0;
        owned.entries.retain(|entry| {
            let overlaps = entry.span.start < span.end && span.start < entry.span.end;
            if overlaps {
                released += entry.frame.bgra.len();
            }
            !overlaps
        });
        owned.used_bytes = owned.used_bytes - released + size;
        owned.entries.push_back(Entry {
            span,
            frame,
            last_used: tick,
        });
        let over_limit = owned.used_bytes > owned.limit_bytes;
        pool.used_bytes = pool.used_bytes - released + size;
        if over_limit {
            while pool
                .owners
                .get(&self.owner)
                .is_some_and(|owned| owned.used_bytes > owned.limit_bytes)
                && pool.evict_oldest_of(self.owner)
            {}
        }
        while pool.used_bytes > pool.capacity_bytes && pool.evict_oldest() {}
    }

    pub fn clear(&mut self) {
        let mut pool = self.pool.lock();
        let Some(owned) = pool.owners.get_mut(&self.owner) else {
            return;
        };
        let released = owned.used_bytes;
        owned.entries.clear();
        owned.used_bytes = 0;
        pool.used_bytes -= released;
    }
}

impl Drop for FrameCache {
    fn drop(&mut self) {
        let mut pool = self.pool.lock();
        if let Some(owned) = pool.owners.remove(&self.owner) {
            pool.used_bytes -= owned.used_bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use tessera_timeline::Time;

    use super::*;

    fn frame(bytes: usize) -> Arc<VideoFrame> {
        Arc::new(VideoFrame {
            width: 1,
            height: 1,
            time: Time::ZERO,
            bgra: vec![0; bytes],
        })
    }

    fn cache(capacity: usize) -> FrameCache {
        FrameCache::new(&FramePool::new(capacity), usize::MAX)
    }

    #[test]
    fn lookups_fall_within_a_frame_span() {
        let mut cache = cache(100);
        let cached = frame(4);
        cache.insert(10..20, cached.clone());
        assert!(cache.get(9).is_none());
        assert!(Arc::ptr_eq(&cache.get(10).unwrap(), &cached));
        assert!(Arc::ptr_eq(&cache.get(19).unwrap(), &cached));
        assert!(cache.get(20).is_none());
    }

    #[test]
    fn least_recently_used_frames_are_evicted_first() {
        let mut cache = cache(12);
        cache.insert(0..1, frame(4));
        cache.insert(1..2, frame(4));
        cache.insert(2..3, frame(4));
        cache.get(0);
        cache.insert(3..4, frame(4));
        assert!(cache.get(0).is_some());
        assert!(cache.get(1).is_none());
        assert!(cache.get(2).is_some());
        assert!(cache.get(3).is_some());
    }

    #[test]
    fn asking_whether_a_frame_is_held_leaves_the_eviction_order_alone() {
        let mut cache = cache(8);
        cache.insert(0..1, frame(4));
        cache.insert(1..2, frame(4));

        assert!(cache.contains(0));
        assert!(!cache.contains(2));

        cache.insert(2..3, frame(4));

        assert!(!cache.contains(0));
        assert!(cache.contains(1));
    }

    #[test]
    fn frames_larger_than_the_capacity_are_not_kept() {
        let mut cache = cache(3);
        cache.insert(0..1, frame(4));
        assert!(cache.get(0).is_none());
    }

    #[test]
    fn overlapping_spans_replace_the_older_entry() {
        let mut cache = cache(8);
        cache.insert(0..i64::MAX, frame(4));
        let replacement = frame(4);
        cache.insert(0..10, replacement.clone());
        assert!(Arc::ptr_eq(&cache.get(5).unwrap(), &replacement));
        assert!(cache.get(10).is_none());
        cache.insert(10..20, frame(4));
        assert!(cache.get(5).is_some());
    }

    #[test]
    fn caches_in_one_pool_share_its_budget_and_evict_each_others_oldest() {
        let pool = FramePool::new(12);
        let mut first = FrameCache::new(&pool, usize::MAX);
        let mut second = FrameCache::new(&pool, usize::MAX);

        first.insert(0..1, frame(4));
        second.insert(0..1, frame(4));
        first.insert(1..2, frame(4));

        assert_eq!(pool.used_bytes(), 12);

        second.get(0);
        second.insert(1..2, frame(4));

        assert!(!first.contains(0));
        assert!(first.contains(1));
        assert!(second.contains(0));
        assert!(second.contains(1));
        assert_eq!(pool.used_bytes(), 12);
    }

    #[test]
    fn a_cache_keeps_within_its_own_limit_too() {
        let pool = FramePool::new(100);
        let mut limited = FrameCache::new(&pool, 8);
        let mut other = FrameCache::new(&pool, usize::MAX);

        other.insert(0..1, frame(4));
        for start in 0..3 {
            limited.insert(start..start + 1, frame(4));
        }

        assert!(!limited.contains(0));
        assert!(limited.contains(2));
        assert!(other.contains(0));
        assert_eq!(pool.used_bytes(), 12);
    }

    #[test]
    fn dropping_or_clearing_a_cache_returns_its_bytes_to_the_pool() {
        let pool = FramePool::new(100);
        let mut first = FrameCache::new(&pool, usize::MAX);
        let mut second = FrameCache::new(&pool, usize::MAX);
        first.insert(0..1, frame(4));
        second.insert(0..1, frame(6));

        first.clear();

        assert_eq!(pool.used_bytes(), 6);

        drop(second);

        assert_eq!(pool.used_bytes(), 0);
    }
}
