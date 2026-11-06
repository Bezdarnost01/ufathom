//! A thread-safe read-through page cache.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use crate::error::{MemError, range_end};
use crate::reader::{Addr, MemoryReader};
use crate::types::{MemoryRegion, ModuleInfo};

/// Size of a cached page in bytes.
pub const PAGE_SIZE: usize = 4096;

/// Default limit on cached pages (64 MiB of page data).
pub const DEFAULT_MAX_PAGES: usize = 16 * 1024;

/// Sorted, disjoint `(start, end)` address intervals.
type Intervals = Vec<(u64, u64)>;

const PAGE_MASK: u64 = PAGE_SIZE as u64 - 1;

/// What the cache knows about one page.
enum Page {
    /// All 4096 bytes were read.
    Full(Box<[u8; PAGE_SIZE]>),
    /// The whole page could not be read, but part of it lies in a readable
    /// region, as happens with sources that are not page aligned. Reads that
    /// touch it go to the inner reader.
    Partial,
    /// No byte of the page is readable.
    Failed,
}

/// Counters describing cache behaviour. Values are a point-in-time snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Page lookups answered from the cache, including cached failures.
    pub hits: u64,
    /// Page lookups that had to read from the inner reader.
    pub misses: u64,
    /// Misses where the inner reader could not supply the whole page.
    pub failed_pages: u64,
    /// Times the cache was emptied to stay under its page limit.
    pub evictions: u64,
}

/// A 4 KiB read-through cache in front of another [`MemoryReader`].
///
/// Pages are read whole from the inner reader on first use and kept, so later
/// reads of nearby data cost no system call. Pages that cannot be read are
/// cached as failures, which makes repeated probing of unmapped memory cheap.
/// The cache is a point-in-time view: it does not notice the target changing.
/// Call [`PageCache::clear`] to refresh it.
///
/// A page that is only partly readable (a source whose regions are not page
/// aligned) is never reported as failed: reads touching it go straight to the
/// inner reader, so the cache returns the same bytes the inner reader would.
///
/// The number of pages is bounded by a configurable limit. When the limit is
/// reached the cache is cleared in one step (simple, predictable, and the
/// counters record it), then refilled by demand.
///
/// `regions` and `modules` are not cached.
pub struct PageCache<M> {
    inner: M,
    max_pages: usize,
    pages: RwLock<HashMap<u64, Arc<Page>>>,
    /// Merged readable intervals `(start, end)` of the inner reader, fetched
    /// the first time a page fails to load.
    readable: Mutex<Option<Arc<Intervals>>>,
    hits: AtomicU64,
    misses: AtomicU64,
    failed_pages: AtomicU64,
    evictions: AtomicU64,
}

impl<M: MemoryReader> PageCache<M> {
    /// Wraps `inner` with the default page limit.
    pub fn new(inner: M) -> Self {
        Self::with_max_pages(inner, DEFAULT_MAX_PAGES)
    }

    /// Wraps `inner`, caching at most `max_pages` pages (at least one).
    pub fn with_max_pages(inner: M, max_pages: usize) -> Self {
        Self {
            inner,
            max_pages: max_pages.max(1),
            pages: RwLock::new(HashMap::new()),
            readable: Mutex::new(None),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            failed_pages: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// The wrapped reader.
    pub fn inner(&self) -> &M {
        &self.inner
    }

    /// Unwraps the cache and returns the inner reader.
    pub fn into_inner(self) -> M {
        self.inner
    }

    /// The configured page limit.
    pub fn max_pages(&self) -> usize {
        self.max_pages
    }

    /// Number of pages currently cached, failures included.
    pub fn cached_pages(&self) -> usize {
        self.pages
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Current counters.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            failed_pages: self.failed_pages.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }

    /// Drops every cached page, including cached failures. Counters are kept.
    pub fn clear(&self) {
        self.pages
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        *self.readable.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Returns the page that starts at `page_base`, loading it when absent.
    fn page(&self, page_base: u64) -> Arc<Page> {
        if let Some(page) = self
            .pages
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&page_base)
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Arc::clone(page);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        // The inner read happens without any lock held, so slow reads of one
        // page never block readers of other pages. Two threads may load the
        // same page; the first insertion wins.
        let loaded = Arc::new(self.load(page_base));
        let mut pages = self.pages.write().unwrap_or_else(PoisonError::into_inner);
        if !pages.contains_key(&page_base) && pages.len() >= self.max_pages {
            pages.clear();
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        Arc::clone(pages.entry(page_base).or_insert(loaded))
    }

    /// Reads one page from the inner reader and classifies the result.
    fn load(&self, page_base: u64) -> Page {
        let mut data = Box::new([0u8; PAGE_SIZE]);
        if self.inner.read_exact(page_base, &mut data[..]).is_ok() {
            return Page::Full(data);
        }
        self.failed_pages.fetch_add(1, Ordering::Relaxed);
        if self.touches_readable_region(page_base) {
            Page::Partial
        } else {
            Page::Failed
        }
    }

    /// Whether any byte of the page lies in a region the inner reader lists as
    /// readable. If the region list cannot be obtained the answer is `true`,
    /// which is always safe: the inner reader decides.
    fn touches_readable_region(&self, page_base: u64) -> bool {
        let Some(intervals) = self.readable_intervals() else {
            return true;
        };
        let page_end = page_base.saturating_add(PAGE_SIZE as u64);
        let before = intervals.partition_point(|(start, _)| *start < page_end);
        before
            .checked_sub(1)
            .and_then(|last| intervals.get(last))
            .is_some_and(|(_, end)| *end > page_base)
    }

    fn readable_intervals(&self) -> Option<Arc<Intervals>> {
        let mut slot = self.readable.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = slot.as_ref() {
            return Some(Arc::clone(existing));
        }
        let regions = self.inner.regions().ok()?;
        let mut spans: Vec<(u64, u64)> = regions
            .iter()
            .filter(|region| region.perms.read)
            .map(|region| (region.base, region.base.saturating_add(region.size)))
            .collect();
        spans.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
        for (start, end) in spans {
            match merged.last_mut() {
                Some((_, last_end)) if start <= *last_end => *last_end = (*last_end).max(end),
                _ => merged.push((start, end)),
            }
        }
        let merged = Arc::new(merged);
        *slot = Some(Arc::clone(&merged));
        Some(merged)
    }
}

impl<M: MemoryReader> MemoryReader for PageCache<M> {
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        range_end(addr, buf.len())?;
        let mut cursor = addr;
        let mut done = 0usize;
        while done < buf.len() {
            let page_base = cursor & !PAGE_MASK;
            let offset = (cursor & PAGE_MASK) as usize;
            let take = (PAGE_SIZE - offset).min(buf.len() - done);
            let fail = || MemError::unreadable(cursor, take);
            let dst = buf.get_mut(done..done + take).ok_or_else(fail)?;
            match &*self.page(page_base) {
                Page::Full(data) => {
                    let src = data.get(offset..offset + take).ok_or_else(fail)?;
                    dst.copy_from_slice(src);
                }
                Page::Partial => self.inner.read_exact(cursor, dst)?,
                Page::Failed => return Err(fail()),
            }
            done += take;
            cursor += take as u64;
        }
        Ok(())
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        self.inner.regions()
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        self.inner.modules()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    use super::*;
    use crate::{MemoryReaderExt, Snapshot};

    /// Wraps a reader and counts calls that reach it.
    struct Counting<M> {
        inner: M,
        reads: AtomicUsize,
    }

    impl<M: MemoryReader> Counting<M> {
        fn new(inner: M) -> Self {
            Self {
                inner,
                reads: AtomicUsize::new(0),
            }
        }
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl<M: MemoryReader> MemoryReader for Counting<M> {
        fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.read_exact(addr, buf)
        }
        fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
            self.inner.regions()
        }
        fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
            self.inner.modules()
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn two_pages() -> Counting<Snapshot> {
        Counting::new(
            Snapshot::builder()
                .region(0x10000, pattern(2 * PAGE_SIZE))
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn hits_and_misses_are_counted() {
        let cache = PageCache::new(two_pages());
        assert_eq!(cache.read_u8(0x10000).unwrap(), 0);
        assert_eq!(cache.read_u8(0x10001).unwrap(), 1);
        assert_eq!(cache.read_u8(0x10fff).unwrap(), (0xfff % 251) as u8);
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.failed_pages, 0);
        assert_eq!(cache.inner().reads(), 1);
        assert_eq!(cache.cached_pages(), 1);
    }

    #[test]
    fn failed_pages_are_cached() {
        let cache = PageCache::new(two_pages());
        for _ in 0..5 {
            assert!(cache.read_u8(0x50000).is_err());
        }
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 4);
        assert_eq!(stats.failed_pages, 1);
        // One whole-page attempt, no repeated probing.
        assert_eq!(cache.inner().reads(), 1);
    }

    #[test]
    fn reads_span_pages_and_match_the_source() {
        let cache = PageCache::new(two_pages());
        let mut expected = vec![0u8; 100];
        cache
            .inner()
            .read_exact(0x10000 + 4096 - 50, &mut expected)
            .unwrap();
        let mut got = vec![0u8; 100];
        cache.read_exact(0x10000 + 4096 - 50, &mut got).unwrap();
        assert_eq!(got, expected);
        assert_eq!(cache.stats().misses, 2);

        // A read of the whole two pages at once.
        let mut all = vec![0u8; 2 * PAGE_SIZE];
        cache.read_exact(0x10000, &mut all).unwrap();
        assert_eq!(all, pattern(2 * PAGE_SIZE));
    }

    #[test]
    fn read_that_ends_in_an_unmapped_page_fails_whole() {
        let cache = PageCache::new(two_pages());
        let mut buf = [0u8; 32];
        let err = cache
            .read_exact(0x10000 + 2 * PAGE_SIZE as u64 - 16, &mut buf)
            .unwrap_err();
        assert!(matches!(err, MemError::Unreadable { .. }), "{err}");
        assert!(matches!(
            cache.read_exact(u64::MAX - 3, &mut buf),
            Err(MemError::AddressOverflow { .. })
        ));
        // The top page itself fails cleanly instead of overflowing.
        assert!(cache.read_exact(u64::MAX - 8, &mut buf[..4]).is_err());
    }

    #[test]
    fn unaligned_regions_stay_readable_through_the_cache() {
        // 100 bytes in the middle of a page, as a minidump might contain.
        let snap = Snapshot::builder()
            .region(0x20100, pattern(100))
            .build()
            .unwrap();
        let cache = PageCache::new(Counting::new(snap));
        let mut got = [0u8; 100];
        cache.read_exact(0x20100, &mut got).unwrap();
        assert_eq!(got.to_vec(), pattern(100));
        assert!(cache.read_u8(0x20164).is_err());
        assert!(cache.read_u8(0x20000).is_err());
        // A different page with no region at all is a cached failure.
        assert!(cache.read_u8(0x30000).is_err());
        let before = cache.inner().reads();
        assert!(cache.read_u8(0x30001).is_err());
        assert_eq!(cache.inner().reads(), before);
    }

    #[test]
    fn eviction_bounds_the_page_count() {
        let snap = Snapshot::builder()
            .region(0x10000, pattern(16 * PAGE_SIZE))
            .build()
            .unwrap();
        let cache = PageCache::with_max_pages(snap, 4);
        for page in 0..16u64 {
            assert_eq!(
                cache.read_u8(0x10000 + page * PAGE_SIZE as u64).unwrap(),
                ((page as usize * PAGE_SIZE) % 251) as u8
            );
            assert!(cache.cached_pages() <= 4);
        }
        let stats = cache.stats();
        assert_eq!(stats.misses, 16);
        assert_eq!(stats.evictions, 3);
        assert_eq!(cache.max_pages(), 4);
    }

    #[test]
    fn clear_forgets_pages_but_keeps_counters() {
        let cache = PageCache::new(two_pages());
        cache.read_u8(0x10000).unwrap();
        cache.clear();
        assert_eq!(cache.cached_pages(), 0);
        cache.read_u8(0x10000).unwrap();
        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn regions_and_modules_delegate() {
        let snap = Snapshot::builder()
            .region(0x1000, [0u8; 8])
            .module("m.dll", 0x1000, 8)
            .build()
            .unwrap();
        let cache = PageCache::new(snap);
        assert_eq!(cache.regions().unwrap().len(), 1);
        assert_eq!(cache.modules().unwrap()[0].name, "m.dll");
    }

    #[test]
    fn concurrent_readers_see_consistent_data() {
        const PAGES: usize = 64;
        let source = pattern(PAGES * PAGE_SIZE);
        let snap = Snapshot::builder()
            .region(0x40000, source.clone())
            .build()
            .unwrap();
        // Small limit so eviction races with reads.
        let cache = Arc::new(PageCache::with_max_pages(snap, 8));
        let handles: Vec<_> = (0..8u64)
            .map(|worker| {
                let cache = Arc::clone(&cache);
                let source = source.clone();
                thread::spawn(move || {
                    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ worker;
                    for _ in 0..2000 {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        let offset = (state % (PAGES * PAGE_SIZE) as u64) as usize;
                        let len = 1 + ((state >> 32) % 6000) as usize;
                        let mut buf = vec![0u8; len];
                        let result = cache.read_exact(0x40000 + offset as u64, &mut buf);
                        if offset + len <= source.len() {
                            result.unwrap();
                            assert_eq!(&buf[..], &source[offset..offset + len]);
                        } else {
                            assert!(result.is_err());
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let stats = cache.stats();
        assert!(stats.hits + stats.misses > 0);
        assert!(cache.cached_pages() <= 8);
    }
}
