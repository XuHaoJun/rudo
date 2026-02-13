use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::LazyLock;

const DEFAULT_HISTORY_SIZE: usize = 1024;

const SUSPICIOUS_THRESHOLD: u64 = 2;

/// Threshold for detecting suspicious young object sweep.
///
/// A young generation object (gen 0) that survives 2+ full GC cycles without being
/// promoted to old generation is suspicious. This typically indicates the anti-pattern:
///   `Vec<Gc<T>>` (non-GC-managed container holding GC pointers)
/// instead of:
///   `Gc<Vec<Gc<T>>>` (GC-managed container)
///
/// Objects in young gen for 2+ cycles that get swept are likely created using this
/// incorrect pattern - the container is not traced by GC, so inner Gc pointers become
/// invisible to the collector.
///
/// Note: This is a debug-only feature gated behind `debug-suspicious-sweep`. The race
/// condition in the ring buffer is acceptable because worst case is detection giving
/// wrong result, not memory corruption.
static GC_CYCLE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[inline]
pub fn get_gc_cycle_id() -> u64 {
    GC_CYCLE_COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[inline]
pub fn current_gc_cycle_id() -> u64 {
    GC_CYCLE_COUNTER.load(Ordering::Relaxed)
}

/// Tracks young object allocations for suspicious sweep detection.
#[allow(clippy::non_send_fields_in_send_ty)]
pub struct YoungObjectHistory {
    records: Vec<YoungObjectRecord>,
    write_idx: AtomicUsize,
    initial_gc_id: AtomicU64,
    max_size: usize,
    enabled: AtomicU64,
}

#[derive(Clone, Copy)]
struct YoungObjectRecord {
    ptr: *const u8,
    gc_id: u64,
}

#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Sync for YoungObjectHistory {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for YoungObjectHistory {}

#[allow(clippy::must_use_candidate)]
impl YoungObjectHistory {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_HISTORY_SIZE)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: vec![
                YoungObjectRecord {
                    ptr: std::ptr::null(),
                    gc_id: 0,
                };
                capacity
            ],
            write_idx: AtomicUsize::new(0),
            initial_gc_id: AtomicU64::new(0),
            max_size: capacity,
            enabled: AtomicU64::new(1),
        }
    }

    pub fn record(&self, ptr: *const u8, gc_id: u64) {
        if !self.is_enabled() {
            return;
        }

        let idx = self.write_idx.fetch_add(1, Ordering::Relaxed) % self.max_size;

        unsafe {
            let record_ptr = self.records.as_ptr().add(idx).cast_mut();
            record_ptr.write(YoungObjectRecord { ptr, gc_id });
        }

        self.initial_gc_id
            .compare_exchange(0, gc_id, Ordering::Relaxed, Ordering::Relaxed)
            .ok();
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) != 0
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(u64::from(enabled), Ordering::Relaxed);
    }

    pub fn is_suspicious(&self, ptr: *const u8, current_gc_id: u64) -> bool {
        if !self.is_enabled() {
            return false;
        }

        if ptr.is_null() {
            return false;
        }

        let initial = self.initial_gc_id.load(Ordering::Relaxed);
        if initial == 0 || current_gc_id < initial {
            return false;
        }

        for record in &self.records {
            if record.ptr == ptr {
                let age = current_gc_id.saturating_sub(record.gc_id);
                if age == 0 {
                    return false;
                }
                return age <= SUSPICIOUS_THRESHOLD;
            }
        }

        false
    }

    pub fn clear(&self) {
        self.write_idx.store(0, Ordering::Relaxed);
        self.initial_gc_id.store(0, Ordering::Relaxed);

        for i in 0..self.max_size {
            unsafe {
                let record_ptr = self.records.as_ptr().add(i).cast_mut();
                record_ptr.write(YoungObjectRecord {
                    ptr: std::ptr::null(),
                    gc_id: 0,
                });
            }
        }
    }
}

impl Default for YoungObjectHistory {
    fn default() -> Self {
        Self::new()
    }
}

static HISTORY: LazyLock<YoungObjectHistory> = LazyLock::new(YoungObjectHistory::new);

#[inline]
pub fn record_young_object(ptr: *const u8) {
    #[cfg(feature = "debug-suspicious-sweep")]
    {
        let gc_id = current_gc_cycle_id();
        HISTORY.record(ptr, gc_id);
    }
}

#[inline]
pub fn is_suspicious_sweep(ptr: *const u8) -> bool {
    #[cfg(feature = "debug-suspicious-sweep")]
    {
        let gc_id = current_gc_cycle_id();
        HISTORY.is_suspicious(ptr, gc_id)
    }
    #[cfg(not(feature = "debug-suspicious-sweep"))]
    {
        let _ = ptr;
        false
    }
}

pub fn set_detection_enabled(enabled: bool) {
    #[cfg(feature = "debug-suspicious-sweep")]
    {
        HISTORY.set_enabled(enabled);
    }
    #[cfg(not(feature = "debug-suspicious-sweep"))]
    {
        let _ = enabled;
    }
}

pub fn is_detection_enabled() -> bool {
    #[cfg(feature = "debug-suspicious-sweep")]
    {
        HISTORY.is_enabled()
    }
    #[cfg(not(feature = "debug-suspicious-sweep"))]
    {
        false
    }
}

pub fn clear_history() {
    #[cfg(feature = "debug-suspicious-sweep")]
    {
        HISTORY.clear();
    }
}

#[cfg(all(test, feature = "debug-suspicious-sweep"))]
mod tests {
    use super::*;

    #[test]
    fn test_is_suspicious_young_object() {
        let history = YoungObjectHistory::with_capacity(16);
        history.set_enabled(true);

        let ptr = 0x1000usize as *const u8;

        history.record(ptr, 1);

        assert!(history.is_suspicious(ptr, 2));
        assert!(history.is_suspicious(ptr, 3));
        assert!(!history.is_suspicious(ptr, 4));
    }

    #[test]
    fn test_is_suspicious_not_recorded() {
        let history = YoungObjectHistory::with_capacity(16);
        history.set_enabled(true);

        let ptr = 0x1000usize as *const u8;

        assert!(!history.is_suspicious(ptr, 5));
    }

    #[test]
    fn test_is_suspicious_disabled() {
        let history = YoungObjectHistory::with_capacity(16);
        history.set_enabled(false);

        let ptr = 0x1000usize as *const u8;
        history.record(ptr, 1);

        assert!(!history.is_suspicious(ptr, 2));
    }

    #[test]
    fn test_is_suspicious_null_ptr() {
        let history = YoungObjectHistory::with_capacity(16);
        history.set_enabled(true);

        assert!(!history.is_suspicious(std::ptr::null(), 5));
    }

    #[test]
    fn test_is_suspicious_initial_gc_id_zero() {
        let history = YoungObjectHistory::new();
        history.set_enabled(true);

        let ptr = 0x1000usize as *const u8;

        assert!(!history.is_suspicious(ptr, 0));
    }
}
