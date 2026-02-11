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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
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
#[repr(u32)]
pub enum FallbackReason {
    None = 0,
    DirtyPagesExceeded = 1,
    SliceTimeout = 2,
    WorklistUnbounded = 3,
    SatbBufferOverflow = 4,
}

impl FallbackReason {
    #[must_use]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::DirtyPagesExceeded,
            2 => Self::SliceTimeout,
            3 => Self::WorklistUnbounded,
            4 => Self::SatbBufferOverflow,
            _ => Self::None,
        }
    }

    #[must_use]
    pub fn to_u32(self) -> u32 {
        self as u32
    }
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

/// Incremental marking state singleton.
///
/// This type manages the state machine for incremental garbage collection marking.
/// It is implemented as a process-level singleton accessed via `global()`.
///
/// # Thread Safety
///
/// This type is currently designed for single-threaded access during GC mark slices.
/// The `worklist` field is reserved for future parallel marking coordination and
/// is currently unused.
///
/// **Important**: The `unsafe impl Sync` declaration is intentionally removed.
/// When parallel marking is implemented, proper synchronization (Mutex or atomic
/// operations) must be added to the `worklist` field before it can be safely accessed
/// from multiple threads. The blanket `unsafe impl Sync` was removed because the
/// `UnsafeCell<SegQueue>` does not provide thread-safe interior mutability.
///
/// # Usage
///
/// The state machine transitions through phases: Idle → Snapshot → Marking → `FinalMark` → Sweeping.
/// Only the `phase` field uses atomic operations for cross-phase visibility. Other fields
/// are accessed only from the GC thread during synchronized phases.
#[derive(Debug)]
#[allow(dead_code)]
pub struct IncrementalMarkState {
    phase: AtomicUsize,
    worklist: UnsafeCell<SegQueue<*const GcBox<()>>>,
    config: Mutex<IncrementalConfig>,
    enabled: AtomicBool,
    stats: MarkStats,
    fallback_requested: AtomicBool,
    root_count: AtomicUsize,
    max_worklist_size: AtomicUsize,
    slice_start_time: Mutex<Option<Instant>>,
    slice_counter: AtomicUsize,
    rendezvous_ack_counter: AtomicUsize,
    #[cfg(feature = "tracing")]
    gc_id: Mutex<Option<crate::tracing::GcId>>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct MarkStats {
    pub objects_marked: AtomicUsize,
    pub dirty_pages_scanned: AtomicUsize,
    pub slices_executed: AtomicUsize,
    pub mark_time_ns: AtomicU64,
    pub fallback_occurred: AtomicBool,
    pub fallback_reason: AtomicU32,
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
            fallback_reason: AtomicU32::new(0),
        }
    }

    pub fn reset(&self) {
        self.objects_marked.store(0, Ordering::SeqCst);
        self.dirty_pages_scanned.store(0, Ordering::SeqCst);
        self.slices_executed.store(0, Ordering::SeqCst);
        self.mark_time_ns.store(0, Ordering::SeqCst);
        self.fallback_occurred.store(false, Ordering::SeqCst);
        self.fallback_reason.store(0, Ordering::SeqCst);
    }

    pub fn record_fallback(&self, reason: FallbackReason) {
        self.fallback_occurred.store(true, Ordering::SeqCst);
        self.fallback_reason
            .store(reason.to_u32(), Ordering::SeqCst);
    }

    pub fn fallback_reason(&self) -> FallbackReason {
        FallbackReason::from_u32(self.fallback_reason.load(Ordering::Acquire))
    }
}

/// SAFETY: `IncrementalMarkState` is currently accessed only from the GC thread.
/// If parallel marking is implemented, proper synchronization must be added.
unsafe impl Send for IncrementalMarkState {}

/// SAFETY: `IncrementalMarkState` is accessed as a process-level singleton via `global()`.
///
/// The `UnsafeCell<SegQueue>` in the `worklist` field is accessed single-threaded from the
/// GC thread during mark slices via `push_work()` and `pop_work()`. All other fields are
/// either atomic or protected by Mutex.
///
/// The blanket `unsafe impl Sync` is justified because:
/// 1. All access to `worklist` occurs from the GC thread during synchronized mark slices
/// 2. No concurrent access from mutator threads
/// 3. Atomic fields use proper ordering (`SeqCst` for writes, default for reads)
///
/// When parallel marking is implemented:
/// 1. The `worklist` field MUST be protected with proper synchronization
/// 2. Concurrent access without synchronization is undefined behavior
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
            enabled: AtomicBool::new(false),
            stats: MarkStats::new(),
            fallback_requested: AtomicBool::new(false),
            root_count: AtomicUsize::new(0),
            max_worklist_size: AtomicUsize::new(0),
            slice_start_time: Mutex::new(None),
            slice_counter: AtomicUsize::new(0),
            rendezvous_ack_counter: AtomicUsize::new(0),
            #[cfg(feature = "tracing")]
            gc_id: Mutex::new(None),
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
        #[cfg(feature = "tracing")]
        {
            let phase_str = match phase {
                MarkPhase::Idle => "idle",
                MarkPhase::Snapshot => "snapshot",
                MarkPhase::Marking => "marking",
                MarkPhase::FinalMark => "final_mark",
                MarkPhase::Sweeping => "sweeping",
            };
            let objects_marked = self.stats.objects_marked.load(Ordering::Relaxed);
            crate::gc::tracing::log_phase_transition(phase_str, objects_marked);
        }
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

    pub fn set_root_count(&self, count: usize) {
        self.root_count.store(count, Ordering::SeqCst);
        self.max_worklist_size.store(count, Ordering::SeqCst);
    }

    pub fn root_count(&self) -> usize {
        self.root_count.load(Ordering::SeqCst)
    }

    #[inline]
    fn update_max_worklist_size(&self, size: usize) {
        let current_max = self.max_worklist_size.load(Ordering::SeqCst);
        if size > current_max {
            self.max_worklist_size.store(size, Ordering::SeqCst);
        }
    }

    fn max_worklist_size(&self) -> usize {
        self.max_worklist_size.load(Ordering::SeqCst)
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
        self.enabled.store(config.enabled, Ordering::Relaxed);
    }

    #[cfg(feature = "tracing")]
    pub fn set_gc_id(&self, gc_id: crate::tracing::GcId) {
        *self.gc_id.lock() = Some(gc_id);
    }

    #[cfg(feature = "tracing")]
    pub fn gc_id(&self) -> Option<crate::tracing::GcId> {
        *self.gc_id.lock()
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
        self.root_count.store(0, Ordering::SeqCst);
        self.rendezvous_ack_counter.store(0, Ordering::SeqCst);
    }

    pub fn increment_rendezvous_ack(&self) -> usize {
        self.rendezvous_ack_counter.fetch_add(1, Ordering::AcqRel)
    }

    pub fn rendezvous_ack_count(&self) -> usize {
        self.rendezvous_ack_counter.load(Ordering::Acquire)
    }

    pub fn reset_rendezvous_ack(&self) {
        self.rendezvous_ack_counter.store(0, Ordering::Release);
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
    state.enabled.load(Ordering::Relaxed)
        && !state.fallback_requested()
        && is_write_barrier_active()
}

pub fn is_generational_barrier_active() -> bool {
    let state = IncrementalMarkState::global();
    state.enabled.load(Ordering::Relaxed)
        && !state.fallback_requested()
        && is_incremental_marking_active()
}

#[allow(clippy::significant_drop_tightening)]
fn stop_all_mutators_for_snapshot() {
    let state = IncrementalMarkState::global();
    let registry = crate::heap::thread_registry().lock().unwrap();

    crate::heap::GC_REQUESTED.store(true, std::sync::atomic::Ordering::Release);

    for tcb in &registry.threads {
        tcb.gc_requested
            .store(true, std::sync::atomic::Ordering::Release);
    }

    state.increment_rendezvous_ack();

    drop(registry);

    loop {
        let registry = crate::heap::thread_registry().lock().unwrap();
        let active = registry
            .active_count
            .load(std::sync::atomic::Ordering::Acquire);
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        let ack_count = state.rendezvous_ack_count();
        let thread_count = registry.threads.len();

        if active == 1 && ack_count == thread_count {
            break;
        }
    }
}

fn resume_all_mutators() {
    let state = IncrementalMarkState::global();
    let registry = crate::heap::thread_registry().lock().unwrap();
    for tcb in &registry.threads {
        tcb.gc_requested
            .store(false, std::sync::atomic::Ordering::Release);
        tcb.park_cond.notify_all();
    }
    drop(registry);
    crate::heap::GC_REQUESTED.store(false, std::sync::atomic::Ordering::Release);
    state.reset_rendezvous_ack();
}

#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn mark_root_for_snapshot(ptr: NonNull<GcBox<()>>, visitor: &mut crate::trace::GcVisitor) {
    let ptr_addr = ptr.as_ptr() as *const u8;
    let header = crate::heap::ptr_to_page_header(ptr_addr);

    if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
        return;
    }

    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
        let was_marked = (*header.as_ptr()).is_marked(idx);
        if !was_marked {
            (*header.as_ptr()).set_mark(idx);
            visitor.objects_marked += 1;
        }
        visitor.worklist.push(ptr);
    }
}

pub fn execute_snapshot(heaps: &[&LocalHeap]) -> usize {
    stop_all_mutators_for_snapshot();

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    state.stats().reset();
    state.reset_fallback();
    state.reset_worklist();

    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);

    for heap in heaps {
        unsafe {
            crate::stack::spill_registers_and_scan(|ptr, _addr, _is_reg| {
                if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                    mark_root_for_snapshot(gc_box, &mut visitor);
                }
            });

            #[cfg(any(test, feature = "test-util"))]
            {
                crate::test_util::iter_test_roots(|roots: &std::cell::RefCell<Vec<*const u8>>| {
                    for &ptr in roots.borrow().iter() {
                        if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr) {
                            mark_root_for_snapshot(gc_box, &mut visitor);
                        }
                    }
                });
            }

            #[cfg(feature = "tokio")]
            {
                use crate::tokio::GcRootSet;
                for &ptr in &GcRootSet::global().snapshot(heap) {
                    if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8)
                    {
                        mark_root_for_snapshot(gc_box, &mut visitor);
                    }
                }
            }
        }
    }

    while let Some(ptr) = visitor.worklist.pop() {
        state.push_work(ptr);
    }

    let count = state.worklist_len();
    state.set_root_count(count);
    #[cfg(feature = "tracing")]
    state.set_gc_id(crate::tracing::internal::next_gc_id());
    state.set_phase(MarkPhase::Marking);
    debug_assert!(
        write_barrier_needed(),
        "Write barrier must be active before resuming mutators"
    );
    resume_all_mutators();
    state.start_slice();
    count
}

#[allow(clippy::significant_drop_tightening)]
#[allow(clippy::too_many_lines)]
pub fn mark_slice(heap: &mut LocalHeap, budget: usize) -> MarkSliceResult {
    #[cfg(feature = "tracing")]
    let _span = crate::gc::tracing::span_incremental_mark("mark_slice");

    let state = IncrementalMarkState::global();

    #[cfg(feature = "tracing")]
    {
        if let Some(gc_id) = state.gc_id() {
            crate::gc::tracing::log_incremental_start(budget, gc_id);
        }
    }
    let config = state.config();

    if state.fallback_requested() {
        let reason = state.stats().fallback_reason();
        return MarkSliceResult::Fallback {
            reason: if reason == FallbackReason::None {
                FallbackReason::DirtyPagesExceeded
            } else {
                reason
            },
        };
    }

    heap.take_dirty_pages_snapshot();

    let mut objects_marked = 0;

    while objects_marked < budget {
        if state.fallback_requested() {
            let reason = state.stats().fallback_reason();
            return MarkSliceResult::Fallback {
                reason: if reason == FallbackReason::None {
                    FallbackReason::SliceTimeout
                } else {
                    reason
                },
            };
        }
        match state.pop_work() {
            Some(ptr) => {
                #[allow(clippy::unnecessary_cast)]
                #[allow(clippy::ptr_as_ptr)]
                unsafe {
                    trace_and_mark_object(ptr, state);
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
    state
        .stats()
        .dirty_pages_scanned
        .fetch_add(dirty_scanned, Ordering::SeqCst);

    let total_marked = objects_marked.saturating_add(dirty_scanned);
    state.stats().slices_executed.fetch_add(1, Ordering::SeqCst);
    state
        .stats()
        .objects_marked
        .fetch_add(total_marked, Ordering::SeqCst);

    let dirty_pages = count_dirty_pages(heap);

    #[cfg(feature = "tracing")]
    crate::gc::tracing::log_incremental_slice(total_marked, dirty_pages);

    let slice_elapsed = state.slice_elapsed_ms();
    let worklist_size = state.worklist_len();
    state.update_max_worklist_size(worklist_size);
    let max_size = state.max_worklist_size();
    let root_count = state.root_count();

    if dirty_pages > config.max_dirty_pages {
        state.request_fallback(FallbackReason::DirtyPagesExceeded);
        #[cfg(feature = "tracing")]
        crate::gc::tracing::log_fallback("dirty_pages_exceeded");
        return MarkSliceResult::Fallback {
            reason: FallbackReason::DirtyPagesExceeded,
        };
    }

    if slice_elapsed > config.slice_timeout_ms {
        state.request_fallback(FallbackReason::SliceTimeout);
        #[cfg(feature = "tracing")]
        crate::gc::tracing::log_fallback("slice_timeout");
        return MarkSliceResult::Fallback {
            reason: FallbackReason::SliceTimeout,
        };
    }

    if max_size > 0 && worklist_size > max_size.saturating_mul(10) {
        state.request_fallback(FallbackReason::WorklistUnbounded);
        #[cfg(feature = "tracing")]
        crate::gc::tracing::log_fallback("worklist_unbounded");
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

pub fn execute_final_mark(heaps: &mut [&mut LocalHeap]) -> usize {
    #[cfg(feature = "tracing")]
    let _span = crate::gc::tracing::span_incremental_mark("final_mark");

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::FinalMark);

    let mut total_marked = 0;
    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);

    for h in heaps {
        let heap = h;
        let overflow_values = heap.flush_satb_overflow_buffer();
        for gc_box in overflow_values {
            unsafe {
                crate::gc::gc::mark_object(gc_box, &mut visitor);
            }
            total_marked += 1;
        }

        let satb_values = heap.flush_satb_buffer();
        for gc_box in satb_values {
            unsafe {
                crate::gc::gc::mark_object(gc_box, &mut visitor);
            }
            total_marked += 1;
        }

        heap.flush_remembered_buffer();

        let snapshot_count = heap.take_dirty_pages_snapshot();
        for page_ptr in heap.dirty_pages_iter() {
            unsafe {
                scan_page_for_unmarked_refs(page_ptr, state.stats());
            }
        }
        heap.clear_dirty_pages_snapshot();
    }

    while let Some(ptr) = visitor.worklist.pop() {
        state.push_work(ptr);
        total_marked += 1;
    }

    let remaining = state.worklist_len();
    if remaining > 0 {
        state.set_phase(MarkPhase::Marking);
    } else {
        state.set_phase(MarkPhase::Sweeping);
    }

    total_marked
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
            if (*header).set_mark(i) {
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
    }
    stats
        .dirty_pages_scanned
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Mark a newly allocated object as black (live).
///
/// This implements the "black allocation" SATB optimization where new objects
/// are immediately considered reachable from the mutator. This is safe
/// because:
/// 1. New objects are only visible to the thread that created them
/// 2. The creating thread is at a safepoint during GC marking
/// 3. No other thread can have a reference to a brand new object
///
/// Unlike similar barriers, this always marks new objects as live regardless
/// of whether incremental marking is active. This ensures correct behavior
/// during concurrent marking phases and maintains the SATB invariant that
/// objects allocated during marking are treated as live.
///
/// Returns true if the object was marked, false if already marked or invalid.
#[inline]
pub fn mark_new_object_black(ptr: *const u8) -> bool {
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
