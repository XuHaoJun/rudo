use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::LazyLock;

const DEFAULT_HISTORY_SIZE: usize = 1024;
const SUSPICIOUS_THRESHOLD: u64 = 2;

static GC_COUNTER: AtomicU64 = AtomicU64::new(1);

#[inline]
fn get_current_gc_id() -> u64 {
    #[cfg(feature = "tracing")]
    {
        crate::tracing::internal::next_gc_id().0
    }
    #[cfg(not(feature = "tracing"))]
    {
        GC_COUNTER.fetch_add(1, Ordering::Relaxed)
    }
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

        if self.initial_gc_id.load(Ordering::Relaxed) == 0 {
            self.initial_gc_id.store(gc_id, Ordering::Relaxed);
        }
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

    pub fn record_count(&self) -> usize {
        let idx = self.write_idx.load(Ordering::Relaxed);
        idx.min(self.max_size)
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
        let gc_id = get_current_gc_id();
        HISTORY.record(ptr, gc_id);
    }
}

#[inline]
pub fn is_suspicious_sweep(ptr: *const u8) -> bool {
    #[cfg(feature = "debug-suspicious-sweep")]
    {
        let gc_id = get_current_gc_id();
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
