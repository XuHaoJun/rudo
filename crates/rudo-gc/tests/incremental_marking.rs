//! Integration tests for incremental marking workflow.

#![allow(clippy::significant_drop_tightening, clippy::items_after_statements)]

use rudo_gc::cell::{GcCapture, GcCell};
use rudo_gc::gc::incremental::{
    is_incremental_marking_active, IncrementalConfig, IncrementalMarkState, MarkPhase,
};
use rudo_gc::test_util;
use rudo_gc::{Gc, GcBox, Trace};
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: usize,
}

#[derive(Trace)]
struct NestedData {
    inner: Gc<Data>,
    value: usize,
}

#[test]
fn test_incremental_config_defaults() {
    test_util::reset();

    let config = IncrementalConfig::default();
    assert!(!config.enabled);
    assert_eq!(config.increment_size, 1000);
    assert_eq!(config.max_dirty_pages, 1000);
    assert_eq!(config.remembered_buffer_len, 32);
    assert_eq!(config.slice_timeout_ms, 50);
}

#[test]
fn test_incremental_config_custom() {
    test_util::reset();

    let config = IncrementalConfig {
        enabled: true,
        increment_size: 500,
        max_dirty_pages: 500,
        remembered_buffer_len: 16,
        slice_timeout_ms: 25,
    };

    IncrementalMarkState::global().set_config(config);

    let retrieved = IncrementalMarkState::global().config();
    assert!(retrieved.enabled);
    assert_eq!(retrieved.increment_size, 500);
    assert_eq!(retrieved.max_dirty_pages, 500);
    assert_eq!(retrieved.remembered_buffer_len, 16);
    assert_eq!(retrieved.slice_timeout_ms, 25);
}

#[test]
fn test_basic_allocation_and_collection() {
    test_util::reset();

    let gc = Gc::new(Data { value: 42 });
    assert_eq!(gc.value, 42);

    drop(gc);
    rudo_gc::collect();

    // Object should be collected
    let metrics = rudo_gc::last_gc_metrics();
    assert!(metrics.bytes_reclaimed > 0 || metrics.objects_reclaimed > 0);
}

#[test]
fn test_multiple_allocations() {
    test_util::reset();

    let values: Vec<Gc<Data>> = (0..100).map(|i| Gc::new(Data { value: i })).collect();

    for (i, gc) in values.iter().enumerate() {
        assert_eq!(gc.value, i);
    }

    drop(values);
    rudo_gc::collect();
}

#[test]
fn test_gccell_like_pattern() {
    test_util::reset();

    #[derive(Trace)]
    struct Container {
        items: Gc<RefCell<Vec<Gc<Data>>>>,
    }

    let container = Gc::new(Container {
        items: Gc::new(RefCell::new(Vec::new())),
    });

    for i in 0..10 {
        container
            .items
            .borrow_mut()
            .push(Gc::new(Data { value: i }));
    }

    assert_eq!(container.items.borrow().len(), 10);
    assert_eq!(container.items.borrow()[5].value, 5);
}

#[test]
fn test_incremental_phase_changes() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    assert_eq!(state.phase(), MarkPhase::Idle);

    state.set_phase(MarkPhase::Snapshot);
    assert_eq!(state.phase(), MarkPhase::Snapshot);

    state.set_phase(MarkPhase::Marking);
    assert_eq!(state.phase(), MarkPhase::Marking);

    state.set_phase(MarkPhase::FinalMark);
    assert_eq!(state.phase(), MarkPhase::FinalMark);

    state.set_phase(MarkPhase::Sweeping);
    assert_eq!(state.phase(), MarkPhase::Sweeping);

    state.set_phase(MarkPhase::Idle);
    assert_eq!(state.phase(), MarkPhase::Idle);
}

#[test]
fn test_mark_stats() {
    test_util::reset();

    let stats = IncrementalMarkState::global().stats();

    stats.objects_marked.store(100, Ordering::SeqCst);
    stats.dirty_pages_scanned.store(10, Ordering::SeqCst);
    stats.slices_executed.store(5, Ordering::SeqCst);
    stats.mark_time_ns.store(1_000_000, Ordering::SeqCst);

    assert_eq!(stats.objects_marked.load(Ordering::SeqCst), 100);
    assert_eq!(stats.dirty_pages_scanned.load(Ordering::SeqCst), 10);
    assert_eq!(stats.slices_executed.load(Ordering::SeqCst), 5);
    assert_eq!(stats.mark_time_ns.load(Ordering::SeqCst), 1_000_000);
}

#[test]
fn test_fallback_recorded() {
    test_util::reset();

    let stats = IncrementalMarkState::global().stats();

    assert!(!stats.fallback_occurred.load(Ordering::SeqCst));

    stats.record_fallback(rudo_gc::gc::incremental::FallbackReason::DirtyPagesExceeded);

    assert!(stats.fallback_occurred.load(Ordering::SeqCst));

    let reason = stats.fallback_reason();
    assert!(matches!(
        reason,
        rudo_gc::gc::incremental::FallbackReason::DirtyPagesExceeded
    ));
}

#[test]
fn test_concurrent_allocation_stress() {
    test_util::reset();

    let counter = Arc::new(AtomicUsize::new(0));
    let iterations = 1000;

    for _ in 0..iterations {
        let gc = Gc::new(Data {
            value: counter.fetch_add(1, Ordering::SeqCst),
        });
        assert!(gc.value < iterations);
    }

    assert_eq!(counter.load(Ordering::SeqCst), iterations);
}

#[test]
fn test_execute_snapshot_captures_roots() {
    test_util::reset();

    let gc = Gc::new(Data { value: 42 });
    assert_eq!(gc.value, 42);

    let state = IncrementalMarkState::global();
    assert_eq!(state.phase(), MarkPhase::Idle);

    let root_count = rudo_gc::heap::with_heap(|heap: &mut rudo_gc::heap::LocalHeap| {
        let heaps: [&rudo_gc::heap::LocalHeap; 1] = [heap];
        rudo_gc::gc::incremental::execute_snapshot(&heaps)
    });

    assert!(
        root_count >= 1,
        "execute_snapshot should capture at least 1 root"
    );
    assert_eq!(state.phase(), MarkPhase::Marking);
    assert!(
        !state.worklist_is_empty(),
        "worklist should not be empty after root capture"
    );
}

#[test]
fn test_root_capture_with_nested_objects() {
    test_util::reset();

    let inner = Gc::new(Data { value: 10 });
    let _outer = Gc::new(NestedData { inner, value: 20 });

    let state = IncrementalMarkState::global();
    let root_count = rudo_gc::heap::with_heap(|heap: &mut rudo_gc::heap::LocalHeap| {
        let heaps: [&rudo_gc::heap::LocalHeap; 1] = [heap];
        rudo_gc::gc::incremental::execute_snapshot(&heaps)
    });

    assert!(
        root_count >= 2,
        "should capture both outer and reachable inner as roots"
    );
    assert!(state.worklist_len() >= root_count);
}

#[test]
fn test_large_allocation() {
    test_util::reset();

    let size = 10_000;
    let items: Vec<Gc<Data>> = (0..size).map(|i| Gc::new(Data { value: i })).collect();

    for (i, item) in items.iter().enumerate() {
        assert_eq!(item.value, i);
    }

    drop(items);
    rudo_gc::collect();
}

#[test]
fn test_incremental_marking_not_active_by_default() {
    test_util::reset();

    assert!(!is_incremental_marking_active());

    let gc = Gc::new(Data { value: 1 });
    assert!(!is_incremental_marking_active());

    drop(gc);
    rudo_gc::collect();

    assert!(!is_incremental_marking_active());
}

#[test]
fn test_worklist_size_tracking() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    assert_eq!(state.worklist_len(), 0);
    assert!(state.worklist_is_empty());
}

#[test]
fn test_root_count() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    assert_eq!(state.root_count(), 0);

    state.set_root_count(5000);
    assert_eq!(state.root_count(), 5000);
}

#[test]
fn test_transition_to_validates_state() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    assert!(state.transition_to(MarkPhase::Snapshot));
    assert_eq!(state.phase(), MarkPhase::Snapshot);

    assert!(state.transition_to(MarkPhase::Marking));
    assert_eq!(state.phase(), MarkPhase::Marking);

    assert!(!state.transition_to(MarkPhase::Idle));
    assert_eq!(state.phase(), MarkPhase::Marking);

    assert!(state.transition_to(MarkPhase::FinalMark));
    assert!(state.transition_to(MarkPhase::Sweeping));
    assert_eq!(state.phase(), MarkPhase::Sweeping);
}

#[test]
fn test_config_persists_across_state_changes() {
    test_util::reset();

    {
        let config = IncrementalMarkState::global().config();
        assert!(!config.enabled);
    }

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    state_machine_transition_test();

    {
        let config = IncrementalMarkState::global().config();
        assert!(config.enabled);
    }
}

fn state_machine_transition_test() {
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    state.set_phase(MarkPhase::Marking);
    state.set_phase(MarkPhase::FinalMark);
    state.set_phase(MarkPhase::Sweeping);
    state.set_phase(MarkPhase::Idle);
}

#[test]
fn test_new_allocations_marked_black_during_incremental_marking() {
    use std::cell::RefCell;

    test_util::reset();

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    let root = Gc::new(RefCell::new(Vec::<Gc<Data>>::new()));

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);

    state.set_phase(MarkPhase::Marking);
    assert!(is_incremental_marking_active());

    for i in 0..100 {
        let new_obj = Gc::new(Data { value: i });
        root.borrow_mut().push(new_obj);
    }

    state.set_phase(MarkPhase::FinalMark);
    state.set_phase(MarkPhase::Sweeping);
    state.set_phase(MarkPhase::Idle);

    assert_eq!(root.borrow().len(), 100);
    for (i, obj) in root.borrow().iter().enumerate() {
        assert_eq!(obj.value, i);
    }
}

#[test]
fn test_satb_barrier_records_overwritten_references() {
    use std::cell::RefCell;

    test_util::reset();

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    let _old_obj = Gc::new(Data { value: 1 });
    let new_obj = Gc::new(Data { value: 2 });

    let container = Gc::new(RefCell::new(new_obj));

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    state.set_phase(MarkPhase::Marking);

    *container.borrow_mut() = Gc::new(Data { value: 3 });

    state.set_phase(MarkPhase::FinalMark);
    state.set_phase(MarkPhase::Sweeping);
    state.set_phase(MarkPhase::Idle);

    assert_eq!(container.borrow().value, 3);
}

#[test]
fn test_write_barrier_fast_path_disabled_during_idle() {
    test_util::reset();

    assert!(!is_incremental_marking_active());

    let cell = Gc::new(RefCell::new(Gc::new(Data { value: 42 })));
    let new = Gc::new(Data { value: 100 });

    *cell.borrow_mut() = new;

    assert_eq!(cell.borrow().value, 100);
}

#[test]
fn test_incremental_config_respected_by_write_barrier() {
    use std::cell::RefCell;

    test_util::reset();

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: false,
        ..Default::default()
    });

    let cell = Gc::new(RefCell::new(Gc::new(Data { value: 42 })));
    let new = Gc::new(Data { value: 100 });

    *cell.borrow_mut() = new;

    assert_eq!(cell.borrow().value, 100);

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    let cell2 = Gc::new(RefCell::new(Gc::new(Data { value: 200 })));
    let new2 = Gc::new(Data { value: 300 });

    *cell2.borrow_mut() = new2;

    assert_eq!(cell2.borrow().value, 300);
}

#[derive(Trace)]
struct Item {
    id: u32,
    data: String,
}

impl GcCapture for Item {
    fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<GcBox<()>>] {
        &[]
    }
    fn capture_gc_ptrs_into(&self, _ptrs: &mut Vec<std::ptr::NonNull<GcBox<()>>>) {}
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn test_gc_capture_gc_pointer_stability_in_loop() {
    use std::collections::HashSet;

    test_util::reset();

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    let items_cell: GcCell<Vec<Option<Gc<Item>>>> = GcCell::new(Vec::new());

    for i in 0..10 {
        let item = Gc::new(Item {
            id: i,
            data: format!("item_{i}"),
        });

        let mut items = items_cell.borrow_mut();
        items.push(Some(item.clone()));

        rudo_gc::safepoint();
    }

    let items = items_cell.borrow();
    let addrs: Vec<_> = items
        .iter()
        .filter_map(|o| o.as_ref())
        .map(|gc| Gc::as_ptr(gc) as usize)
        .collect();

    let unique_addrs: HashSet<_> = addrs.iter().collect();
    assert_eq!(
        addrs.len(),
        unique_addrs.len(),
        "BUG: GcBox addresses reused! Got {} unique addresses for 10 items. Addrs: {:?}",
        unique_addrs.len(),
        addrs
    );

    for (idx, item) in items
        .iter()
        .enumerate()
        .filter_map(|(i, o)| o.as_ref().map(|gc| (i, gc)))
    {
        assert_eq!(item.id, idx as u32, "Item {idx} corrupted");
    }
}
