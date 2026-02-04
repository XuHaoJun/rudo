//! Incremental marking for major GC.
//!
//! This module implements incremental marking to reduce GC pause times by splitting
//! the mark phase into smaller cooperative increments that interleave with mutator execution.
//! Uses a hybrid SATB (Snapshot-At-The-Beginning) + insertion-barrier approach.

#![allow(
    missing_docs,
    clippy::missing_panics_doc,
    clippy::new_without_default,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::ptr_cast_constness,
    clippy::unnecessary_cast,
    clippy::ptr_as_ptr
)]

use crossbeam::queue::SegQueue;
use parking_lot::Mutex;
use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

use crate::heap::{LocalHeap, PageHeader};
pub use crate::ptr::GcBox;

pub const DEFAULT_INCREMENT_SIZE: usize = 1000;
pub const DEFAULT_MAX_DIRTY_PAGES: usize = 1000;
pub const DEFAULT_REMEMBERED_BUFFER_LEN: usize = 32;
pub const DEFAULT_SLICE_TIMEOUT_MS: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum MarkPhase {
    Idle = 0,
    Snapshot = 1,
    Marking = 2,
    FinalMark = 3,
    Sweeping = 4,
}

impl MarkPhase {
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    #[allow(clippy::use_self)]
    pub fn from_usize(v: usize) -> Option<Self> {
        match v {
            0 => Some(Self::Idle),
            1 => Some(Self::Snapshot),
            2 => Some(Self::Marking),
            3 => Some(Self::FinalMark),
            4 => Some(Self::Sweeping),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    DirtyPagesExceeded,
    SliceTimeout,
    WorklistUnbounded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkSliceResult {
    Pending {
        objects_marked: usize,
        dirty_pages_remaining: usize,
    },
    Complete {
        total_objects_marked: usize,
        total_slices: usize,
    },
    Fallback {
        reason: FallbackReason,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct IncrementalConfig {
    pub enabled: bool,
    pub increment_size: usize,
    pub max_dirty_pages: usize,
    pub remembered_buffer_len: usize,
    pub slice_timeout_ms: u64,
}

impl Default for IncrementalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            increment_size: DEFAULT_INCREMENT_SIZE,
            max_dirty_pages: DEFAULT_MAX_DIRTY_PAGES,
            remembered_buffer_len: DEFAULT_REMEMBERED_BUFFER_LEN,
            slice_timeout_ms: DEFAULT_SLICE_TIMEOUT_MS,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct IncrementalMarkState {
    phase: AtomicUsize,
    worklist: UnsafeCell<SegQueue<*const GcBox<()>>>,
    config: Mutex<IncrementalConfig>,
    stats: MarkStats,
    fallback_requested: AtomicBool,
    initial_worklist_size: AtomicUsize,
    slice_start_time: Mutex<Option<Instant>>,
    slice_counter: AtomicUsize,
    /// Reserved for future parallel marking coordination. Currently unused.
    workers_at_barrier: AtomicUsize,
    /// Reserved for future parallel marking coordination. Currently unused.
    total_workers: AtomicUsize,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct MarkStats {
    pub objects_marked: AtomicUsize,
    pub dirty_pages_scanned: AtomicUsize,
    pub slices_executed: AtomicUsize,
    pub mark_time_ns: AtomicU64,
    pub fallback_occurred: AtomicBool,
    pub fallback_reason: Mutex<Option<FallbackReason>>,
}

impl Default for MarkStats {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkStats {
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new() -> Self {
        Self {
            objects_marked: AtomicUsize::new(0),
            dirty_pages_scanned: AtomicUsize::new(0),
            slices_executed: AtomicUsize::new(0),
            mark_time_ns: AtomicU64::new(0),
            fallback_occurred: AtomicBool::new(false),
            fallback_reason: Mutex::new(None),
        }
    }

    pub fn reset(&self) {
        self.objects_marked.store(0, Ordering::SeqCst);
        self.dirty_pages_scanned.store(0, Ordering::SeqCst);
        self.slices_executed.store(0, Ordering::SeqCst);
        self.mark_time_ns.store(0, Ordering::SeqCst);
        self.fallback_occurred.store(false, Ordering::SeqCst);
        *self.fallback_reason.lock() = None;
    }

    pub fn record_fallback(&self, reason: FallbackReason) {
        self.fallback_occurred.store(true, Ordering::SeqCst);
        *self.fallback_reason.lock() = Some(reason);
    }
}

unsafe impl Send for IncrementalMarkState {}
unsafe impl Sync for IncrementalMarkState {}

impl Default for IncrementalMarkState {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalMarkState {
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new() -> Self {
        Self {
            phase: AtomicUsize::new(MarkPhase::Idle as usize),
            worklist: UnsafeCell::new(SegQueue::new()),
            config: Mutex::new(IncrementalConfig::default()),
            stats: MarkStats::new(),
            fallback_requested: AtomicBool::new(false),
            initial_worklist_size: AtomicUsize::new(0),
            slice_start_time: Mutex::new(None),
            slice_counter: AtomicUsize::new(0),
            workers_at_barrier: AtomicUsize::new(0),
            total_workers: AtomicUsize::new(0),
        }
    }

    #[must_use]
    pub fn global() -> &'static Self {
        static INSTANCE: LazyLock<IncrementalMarkState> = LazyLock::new(IncrementalMarkState::new);
        &INSTANCE
    }

    pub fn start_slice(&self) {
        *self.slice_start_time.lock() = Some(Instant::now());
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::option_if_let_else)]
    #[allow(clippy::significant_drop_in_scrutinee)]
    pub fn slice_elapsed_ms(&self) -> u64 {
        if let Some(start) = *self.slice_start_time.lock() {
            start.elapsed().as_millis() as u64
        } else {
            0
        }
    }

    pub fn phase(&self) -> MarkPhase {
        let p = self.phase.load(Ordering::SeqCst);
        MarkPhase::from_usize(p).unwrap_or(MarkPhase::Idle)
    }

    pub fn set_phase(&self, phase: MarkPhase) {
        self.phase.store(phase as usize, Ordering::SeqCst);
    }

    pub fn transition_to(&self, new_phase: MarkPhase) -> bool {
        let current = self.phase();
        if !self.is_valid_transition(current, new_phase) {
            return false;
        }
        self.set_phase(new_phase);
        true
    }

    #[allow(clippy::unused_self)]
    #[allow(clippy::missing_const_for_fn)]
    fn is_valid_transition(&self, from: MarkPhase, to: MarkPhase) -> bool {
        matches!(
            (from, to),
            (MarkPhase::Idle, MarkPhase::Snapshot)
                | (
                    MarkPhase::Snapshot | MarkPhase::Marking | MarkPhase::FinalMark,
                    MarkPhase::Marking
                )
                | (MarkPhase::Marking, MarkPhase::FinalMark)
                | (MarkPhase::FinalMark, MarkPhase::Sweeping)
                | (MarkPhase::Sweeping, MarkPhase::Idle)
        )
    }

    fn worklist(&self) -> &SegQueue<*const GcBox<()>> {
        unsafe { &*self.worklist.get() }
    }

    #[allow(clippy::mut_from_ref)]
    fn worklist_mut(&self) -> &mut SegQueue<*const GcBox<()>> {
        unsafe { &mut *self.worklist.get() }
    }

    pub fn push_work(&self, ptr: NonNull<GcBox<()>>) {
        self.worklist().push(ptr.as_ptr());
    }

    #[allow(clippy::missing_panics_doc)]
    pub fn pop_work(&self) -> Option<NonNull<GcBox<()>>> {
        self.worklist()
            .pop()
            .map(|p| NonNull::new(p as *mut GcBox<()>).unwrap())
    }

    pub fn worklist_is_empty(&self) -> bool {
        self.worklist().is_empty()
    }

    pub fn worklist_len(&self) -> usize {
        self.worklist().len()
    }

    pub fn reset_worklist(&self) {
        *self.worklist_mut() = SegQueue::new();
    }

    pub fn request_fallback(&self, reason: FallbackReason) {
        self.fallback_requested.store(true, Ordering::SeqCst);
        self.stats.record_fallback(reason);
    }

    pub fn fallback_requested(&self) -> bool {
        self.fallback_requested.load(Ordering::SeqCst)
    }

    pub fn reset_fallback(&self) {
        self.fallback_requested.store(false, Ordering::SeqCst);
    }

    pub fn set_initial_worklist_size(&self, size: usize) {
        self.initial_worklist_size.store(size, Ordering::SeqCst);
    }

    pub fn initial_worklist_size(&self) -> usize {
        self.initial_worklist_size.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn set_total_workers(&self, count: usize) {
        self.total_workers.store(count, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    pub fn total_workers(&self) -> usize {
        self.total_workers.load(Ordering::SeqCst)
    }

    // ========================================================================
    // Barrier coordination for future parallel marking
    // ========================================================================
    // These functions are reserved for parallel marking where multiple worker
    // threads need to synchronize at slice boundaries. Currently unused
    // because incremental marking runs single-threaded (mutator + GC thread).

    #[allow(dead_code)]
    pub fn wait_at_barrier(&self) {
        self.workers_at_barrier.fetch_add(1, Ordering::SeqCst);
        while self.workers_at_barrier.load(Ordering::Acquire) < self.total_workers() {
            std::hint::spin_loop();
        }
    }

    #[allow(dead_code)]
    pub fn reset_barrier(&self) {
        self.workers_at_barrier.store(0, Ordering::SeqCst);
    }

    pub fn slice_counter(&self) -> usize {
        self.slice_counter.load(Ordering::SeqCst)
    }

    pub fn increment_slice_counter(&self) -> usize {
        self.slice_counter.fetch_add(1, Ordering::SeqCst)
    }

    pub fn config(&self) -> parking_lot::MutexGuard<'_, IncrementalConfig> {
        self.config.lock()
    }

    pub fn set_config(&self, config: IncrementalConfig) {
        *self.config.lock() = config;
    }

    pub fn stats(&self) -> &MarkStats {
        &self.stats
    }

    pub fn reset(&self) {
        self.set_phase(MarkPhase::Idle);
        self.reset_worklist();
        self.reset_fallback();
        self.stats().reset();
        *self.slice_start_time.lock() = None;
    }
}

pub fn is_incremental_marking_active() -> bool {
    let state = IncrementalMarkState::global();
    let phase = state.phase();
    phase == MarkPhase::Snapshot || phase == MarkPhase::Marking || phase == MarkPhase::FinalMark
}

pub fn is_write_barrier_active() -> bool {
    let state = IncrementalMarkState::global();
    let phase = state.phase();
    phase == MarkPhase::Marking
}

pub fn write_barrier_needed() -> bool {
    let state = IncrementalMarkState::global();
    state.config().enabled && !state.fallback_requested() && is_write_barrier_active()
}

pub fn execute_snapshot(heap: &mut LocalHeap) -> usize {
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    state.stats().reset();
    state.reset_fallback();
    state.set_initial_worklist_size(state.worklist_len());
    state.reset_worklist();

    state.reset_barrier();
    state.start_slice();

    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);
    unsafe {
        crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                crate::gc::gc::mark_object(gc_box, &mut visitor);
            }
        });

        #[cfg(any(test, feature = "test-util"))]
        {
            crate::test_util::iter_test_roots(|roots: &std::cell::RefCell<Vec<*const u8>>| {
                for &ptr in roots.borrow().iter() {
                    if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                        crate::gc::gc::mark_object(gc_box, &mut visitor);
                    }
                }
            });
        }

        #[cfg(feature = "tokio")]
        {
            use crate::tokio::GcRootSet;
            for &ptr in &GcRootSet::global().snapshot(heap) {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                    crate::gc::gc::mark_object(gc_box, &mut visitor);
                }
            }
        }
    }

    while let Some(ptr) = visitor.worklist.pop() {
        state.push_work(ptr);
    }

    let root_count = state.worklist_len();
    state.set_phase(MarkPhase::Marking);
    root_count
}

#[allow(clippy::significant_drop_tightening)]
pub fn mark_slice(heap: &mut LocalHeap, budget: usize) -> MarkSliceResult {
    let state = IncrementalMarkState::global();
    let config = state.config();

    if state.fallback_requested() {
        let reason = state.stats().fallback_reason.lock().clone();
        return MarkSliceResult::Fallback {
            reason: reason.unwrap_or(FallbackReason::DirtyPagesExceeded),
        };
    }

    let snapshot_count = heap.take_dirty_pages_snapshot();

    let mut objects_marked = 0;

    while objects_marked < budget {
        if state.fallback_requested() {
            return MarkSliceResult::Fallback {
                reason: state
                    .stats()
                    .fallback_reason
                    .lock()
                    .clone()
                    .unwrap_or(FallbackReason::SliceTimeout),
            };
        }
        match state.pop_work() {
            Some(ptr) => {
                #[allow(clippy::unnecessary_cast)]
                #[allow(clippy::ptr_as_ptr)]
                unsafe {
                    if let Some(gc_box) = NonNull::new(ptr.as_ptr() as *mut GcBox<()>) {
                        trace_and_mark_object(gc_box, state);
                    }
                }
                objects_marked += 1;
            }
            None => {
                break;
            }
        }
    }

    let mut dirty_scanned = 0;
    for page_ptr in heap.dirty_pages_iter() {
        unsafe {
            dirty_scanned += scan_page_for_marked_refs(page_ptr, state);
        }
    }
    heap.clear_dirty_pages_snapshot();

    let additional_marked = state.worklist_len().saturating_sub(objects_marked);
    objects_marked = objects_marked.min(state.worklist_len());
    state.stats().slices_executed.fetch_add(1, Ordering::SeqCst);
    state
        .stats()
        .objects_marked
        .fetch_add(objects_marked, Ordering::SeqCst);

    let dirty_pages = count_dirty_pages(heap);

    let slice_elapsed = state.slice_elapsed_ms();
    let worklist_size = state.worklist_len();
    let initial_size = state.initial_worklist_size();

    if dirty_pages > config.max_dirty_pages {
        state.request_fallback(FallbackReason::DirtyPagesExceeded);
        return MarkSliceResult::Fallback {
            reason: FallbackReason::DirtyPagesExceeded,
        };
    }

    if slice_elapsed > config.slice_timeout_ms {
        state.request_fallback(FallbackReason::SliceTimeout);
        return MarkSliceResult::Fallback {
            reason: FallbackReason::SliceTimeout,
        };
    }

    if initial_size > 0 && worklist_size > initial_size * 10 {
        state.request_fallback(FallbackReason::WorklistUnbounded);
        return MarkSliceResult::Fallback {
            reason: FallbackReason::WorklistUnbounded,
        };
    }

    let remaining_dirty = count_dirty_pages(heap);
    if state.worklist_is_empty() && remaining_dirty == 0 {
        MarkSliceResult::Complete {
            total_objects_marked: state.stats().objects_marked.load(Ordering::SeqCst),
            total_slices: state.stats().slices_executed.load(Ordering::SeqCst),
        }
    } else {
        MarkSliceResult::Pending {
            objects_marked,
            dirty_pages_remaining: remaining_dirty,
        }
    }
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn trace_and_mark_object(gc_box: NonNull<GcBox<()>>, state: &IncrementalMarkState) {
    let ptr = gc_box.as_ptr() as *const u8;
    let header = crate::heap::ptr_to_page_header(ptr);
    let block_size = (*header.as_ptr()).block_size as usize;
    let header_size = crate::heap::PageHeader::header_size(block_size);
    let data_ptr = ptr.add(header_size);

    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);

    ((*gc_box.as_ptr()).trace_fn)(data_ptr, &mut visitor);

    while let Some(child_ptr) = visitor.worklist.pop() {
        state.push_work(child_ptr);
    }
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn scan_page_for_marked_refs(
    page: NonNull<PageHeader>,
    state: &IncrementalMarkState,
) -> usize {
    let header = page.as_ptr();
    let block_size = (*header).block_size as usize;
    let header_size = crate::heap::PageHeader::header_size(block_size);
    let obj_count = (*header).obj_count as usize;
    let mut refs_found = 0;

    for i in 0..obj_count {
        if (*header).is_allocated(i) && !(*header).is_marked(i) {
            let obj_ptr = header.cast::<u8>().add(header_size + i * block_size);
            refs_found += 1;
            if let Some(idx) = crate::heap::ptr_to_object_index(obj_ptr.cast()) {
                if !(*header).is_marked(idx) {
                    (*header).set_mark(idx);
                    #[allow(clippy::cast_ptr_alignment)]
                    #[allow(clippy::unnecessary_cast)]
                    #[allow(clippy::ptr_as_ptr)]
                    let gc_box_ptr = obj_ptr.cast::<GcBox<()>>();
                    if let Some(gc_box) = NonNull::new(gc_box_ptr as *mut GcBox<()>) {
                        state.push_work(gc_box);
                    }
                }
            }
        }
    }
    refs_found
}

pub fn incremental_mark_slice(heap: &mut LocalHeap, budget: usize) -> MarkSliceResult {
    mark_slice(heap, budget)
}

pub fn count_dirty_pages(heap: &LocalHeap) -> usize {
    heap.dirty_pages_count()
}

pub fn take_dirty_pages_snapshot(heap: &mut LocalHeap) -> usize {
    heap.take_dirty_pages_snapshot()
}

pub fn clear_dirty_pages_snapshot(heap: &mut LocalHeap) {
    heap.clear_dirty_pages_snapshot();
}

pub fn execute_final_mark(heap: &mut LocalHeap) -> bool {
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::FinalMark);

    let mut remaining = state.worklist_len();

    // Process SATB buffer: mark all captured old values
    let satb_values = heap.flush_satb_buffer();
    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);
    for gc_box in satb_values {
        unsafe {
            crate::gc::gc::mark_object(gc_box, &mut visitor);
        }
    }

    let snapshot_count = heap.take_dirty_pages_snapshot();
    for page_ptr in heap.dirty_pages_iter() {
        unsafe {
            scan_page_for_unmarked_refs(page_ptr, state.stats());
        }
    }
    heap.clear_dirty_pages_snapshot();
    remaining += state.worklist_len();

    if remaining > 0 {
        state.set_phase(MarkPhase::Marking);
        return false;
    }

    state.set_phase(MarkPhase::Sweeping);
    true
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn scan_page_for_unmarked_refs(page: NonNull<PageHeader>, stats: &MarkStats) {
    let header = page.as_ptr();
    let block_size = (*header).block_size as usize;
    let header_size = crate::heap::PageHeader::header_size(block_size);
    let obj_count = (*header).obj_count as usize;

    for i in 0..obj_count {
        if (*header).is_allocated(i) && !(*header).is_marked(i) {
            let obj_ptr = header.cast::<u8>().add(header_size + i * block_size);
            #[allow(clippy::cast_ptr_alignment)]
            #[allow(clippy::unnecessary_cast)]
            #[allow(clippy::ptr_as_ptr)]
            let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
            if let Some(gc_box) = NonNull::new(gc_box_ptr) {
                let ptr = IncrementalMarkState::global();
                ptr.push_work(gc_box);
            }
        }
    }
    stats
        .dirty_pages_scanned
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Mark a newly allocated object as black (live) during incremental marking.
///
/// This implements the "black allocation" optimization where new objects
/// are immediately considered reachable from the mutator. This is safe
/// because:
/// 1. New objects are only visible to the thread that created them
/// 2. The creating thread is at a safepoint during GC marking
/// 3. No other thread can have a reference to a brand new object
///
/// Returns true if the object was marked, false if marking is not active.
#[inline]
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    if !is_incremental_marking_active() {
        return false;
    }

    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            if !(*header.as_ptr()).is_marked(idx) {
                (*header.as_ptr()).set_mark(idx);
                return true;
            }
        }
    }
    false
}

/// Get the object index for a pointer and mark it black.
///
/// Returns the index if successful, None otherwise.
#[inline]
#[allow(clippy::missing_safety_doc)]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
        let header = crate::heap::ptr_to_page_header(ptr);
        if !(*header.as_ptr()).is_marked(idx) {
            (*header.as_ptr()).set_mark(idx);
            return Some(idx);
        }
    }
    None
}
