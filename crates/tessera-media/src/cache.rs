use std::{collections::VecDeque, ops::Range, sync::Arc};

use crate::VideoFrame;

pub struct FrameCache {
    capacity_bytes: usize,
    used_bytes: usize,
    entries: VecDeque<Entry>,
}

struct Entry {
    span: Range<i64>,
    frame: Arc<VideoFrame>,
}

impl FrameCache {
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            used_bytes: 0,
            entries: VecDeque::new(),
        }
    }

    pub fn get(&mut self, ts: i64) -> Option<Arc<VideoFrame>> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.span.contains(&ts))?;
        let entry = self.entries.remove(index)?;
        let frame = entry.frame.clone();
        self.entries.push_back(entry);
        Some(frame)
    }

    pub fn insert(&mut self, span: Range<i64>, frame: Arc<VideoFrame>) {
        let size = frame.bgra.len();
        if size > self.capacity_bytes || span.is_empty() {
            return;
        }
        self.entries.retain(|entry| {
            let overlaps = entry.span.start < span.end && span.start < entry.span.end;
            if overlaps {
                self.used_bytes -= entry.frame.bgra.len();
            }
            !overlaps
        });
        self.used_bytes += size;
        self.entries.push_back(Entry { span, frame });
        while self.used_bytes > self.capacity_bytes {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.used_bytes -= evicted.frame.bgra.len();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.used_bytes = 0;
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

    #[test]
    fn lookups_fall_within_a_frame_span() {
        let mut cache = FrameCache::new(100);
        let cached = frame(4);
        cache.insert(10..20, cached.clone());
        assert!(cache.get(9).is_none());
        assert!(Arc::ptr_eq(&cache.get(10).unwrap(), &cached));
        assert!(Arc::ptr_eq(&cache.get(19).unwrap(), &cached));
        assert!(cache.get(20).is_none());
    }

    #[test]
    fn least_recently_used_frames_are_evicted_first() {
        let mut cache = FrameCache::new(12);
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
    fn frames_larger_than_the_capacity_are_not_kept() {
        let mut cache = FrameCache::new(3);
        cache.insert(0..1, frame(4));
        assert!(cache.get(0).is_none());
    }

    #[test]
    fn overlapping_spans_replace_the_older_entry() {
        let mut cache = FrameCache::new(8);
        cache.insert(0..i64::MAX, frame(4));
        let replacement = frame(4);
        cache.insert(0..10, replacement.clone());
        assert!(Arc::ptr_eq(&cache.get(5).unwrap(), &replacement));
        assert!(cache.get(10).is_none());
        cache.insert(10..20, frame(4));
        assert!(cache.get(5).is_some());
    }
}
