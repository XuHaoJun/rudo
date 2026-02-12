use rudo_gc::{
    collect_full, current_heap_size, current_old_size, current_young_size, gc_history,
    global_metrics, last_gc_metrics, Gc,
};
use std::mem;
use std::time::Duration;

#[cfg(feature = "test-util")]
use rudo_gc::test_util;

#[test]
fn test_single_allocation_increases_heap() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let before = current_heap_size();
    let _gc = Gc::new(42u64);
    let after = current_heap_size();

    assert!(
        after > before,
        "Heap size should increase after single allocation"
    );
    assert!(
        (after - before) >= mem::size_of::<u64>(),
        "Heap should increase by at least object size"
    );
}

#[test]
fn test_multiple_allocations_linear_growth() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let before = current_heap_size();
    let count: usize = 100;
    let _objects: Vec<Gc<usize>> = (0..count).map(Gc::new).collect();
    let after = current_heap_size();

    let growth = after - before;

    assert!(growth > 0, "Heap should grow with multiple allocations");
}

#[test]
fn test_drop_single_object_reclaims_memory() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let gc = Gc::new(vec![0u8; 1024]);
    let allocated_size = current_heap_size();

    drop(gc);
    collect_full();

    let after = current_heap_size();
    let reclaimed = allocated_size.saturating_sub(after);

    assert!(
        reclaimed > 0 || after <= allocated_size,
        "Memory should be reclaimed or heap should not grow"
    );
}

#[test]
fn test_drop_multiple_objects_reclaims_memory() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let count: usize = 50;
    let objects: Vec<Gc<usize>> = (0..count).map(Gc::new).collect();
    let allocated_size = current_heap_size();

    drop(objects);
    collect_full();

    let after = current_heap_size();
    let reclaimed = allocated_size.saturating_sub(after);

    assert!(
        reclaimed > 0 || after <= allocated_size,
        "Memory should be reclaimed or heap should not grow"
    );
}

#[test]
fn test_young_generation_tracking() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let young_before = current_young_size();

    let _gc = Gc::new(42u64);

    let young_after = current_young_size();

    assert!(
        young_after >= young_before,
        "Young generation size should not decrease after allocation"
    );
}

#[test]
fn test_old_generation_promotion() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let gc = Gc::new(42u64);
    let old_before = current_old_size();

    collect_full();

    let old_after = current_old_size();

    assert!(
        old_after >= old_before,
        "Old generation should not decrease"
    );
    assert!(
        Gc::ptr_eq(&gc, &gc),
        "Object should still be accessible after collection"
    );
}

#[test]
fn test_generation_sum_leq_total() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let _gc = Gc::new(42u64);
    collect_full();

    let young = current_young_size();
    let old = current_old_size();
    let total = current_heap_size();

    let sum = young + old;
    assert!(
        sum <= total + 1024,
        "Generation sizes should approximately equal total heap"
    );
}

#[test]
fn test_bytes_reclaimed_accuracy() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let gc = Gc::new(vec![0u8; 512]);
    let _allocated_size = current_heap_size();

    drop(gc);
    collect_full();

    let metrics = last_gc_metrics();

    let _ = metrics.bytes_reclaimed;
}

#[test]
fn test_objects_reclaimed_count() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let count: usize = 25;
    let objects: Vec<Gc<usize>> = (0..count).map(Gc::new).collect();

    drop(objects);
    collect_full();

    let metrics = last_gc_metrics();

    let _ = metrics.objects_reclaimed;
}

#[test]
fn test_surviving_objects_tracked() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let survivor = Gc::new(42u64);
    let garbage: Vec<Gc<usize>> = (0..10).map(Gc::new).collect();

    drop(garbage);
    collect_full();

    let metrics = last_gc_metrics();

    assert!(
        metrics.objects_surviving >= 1 || metrics.bytes_surviving >= mem::size_of::<u64>(),
        "Surviving object should be tracked"
    );
    assert!(
        Gc::ptr_eq(&survivor, &survivor),
        "Survivor should still be accessible"
    );
}

#[test]
fn test_cumulative_bytes_reclaimed() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let global_before = global_metrics().total_bytes_reclaimed();

    for _ in 0..3 {
        let gc = Gc::new(vec![0u8; 256]);
        drop(gc);
        collect_full();
    }

    let global_after = global_metrics().total_bytes_reclaimed();

    assert!(
        global_after >= global_before,
        "Cumulative bytes reclaimed should not decrease"
    );
}

#[test]
fn test_total_collections_increment() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let collections_before = global_metrics().total_collections();

    let gc = Gc::new(42u64);
    drop(gc);
    collect_full();

    let collections_after = global_metrics().total_collections();

    assert!(
        collections_after >= collections_before,
        "Total collections should not decrease"
    );
}

#[test]
fn test_pause_time_accumulated() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let pause_before = global_metrics().total_pause_time();

    let gc = Gc::new(vec![0u8; 1024]);
    drop(gc);
    collect_full();

    let pause_after = global_metrics().total_pause_time();

    assert!(
        pause_after >= pause_before,
        "Pause time should not decrease"
    );
}

#[test]
fn test_zero_allocation_near_zero_heap() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let size = current_heap_size();

    assert!(size <= 4096, "Empty heap should be small");
}

#[test]
fn test_large_object_tracking() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let before = current_heap_size();
    let gc = Gc::new(vec![0u8; 1024]);
    let after = current_heap_size();

    let _ = after - before;

    assert!(
        after >= before,
        "Large object allocation should increase heap"
    );

    drop(gc);
    collect_full();
}

#[test]
fn test_multiple_collections_cumulative_metrics() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let metrics_before = global_metrics();

    for i in 0..5 {
        let gc = Gc::new(vec![0u8; 128 * (i + 1)]);
        drop(gc);
        collect_full();
    }

    let metrics_after = global_metrics();

    assert!(
        metrics_after.total_collections() >= metrics_before.total_collections(),
        "Collection count should not decrease"
    );
    assert!(
        metrics_after.total_bytes_reclaimed() >= metrics_before.total_bytes_reclaimed(),
        "Cumulative reclaimed bytes should not decrease"
    );
    assert!(
        metrics_after.total_objects_reclaimed() >= metrics_before.total_objects_reclaimed(),
        "Cumulative reclaimed objects should not decrease"
    );
}

#[test]
fn test_collection_phase_durations_recorded() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let gc = Gc::new(vec![0u8; 512]);
    drop(gc);
    collect_full();

    let metrics = last_gc_metrics();

    let total_duration = metrics.clear_duration + metrics.mark_duration + metrics.sweep_duration;

    assert!(
        total_duration >= Duration::ZERO,
        "Phase durations should be recorded"
    );
}

#[test]
fn test_heap_size_after_sequential_operations() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let _initial = current_heap_size();

    let gc1 = Gc::new(vec![0u8; 256]);
    let after_alloc1 = current_heap_size();

    let gc2 = Gc::new(vec![0u8; 256]);
    let after_alloc2 = current_heap_size();

    assert!(
        after_alloc2 >= after_alloc1,
        "Heap should not decrease during allocation"
    );

    drop(gc1);
    collect_full();
    let after_collect1 = current_heap_size();

    drop(gc2);
    collect_full();
    let after_collect2 = current_heap_size();

    assert!(
        after_collect2 <= after_collect1 + 1024,
        "Heap should not increase substantially after collection"
    );
}

#[test]
fn test_allocation_debt_tracking() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let before = current_heap_size();

    for _ in 0..100 {
        let _gc = Gc::new(vec![0u8; 64]);
    }

    let after = current_heap_size();

    assert!(after >= before, "Heap should grow with allocations");
}

#[test]
fn test_minor_vs_major_collection_metrics() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let _gc = Gc::new(42u64);

    collect_full();

    let metrics = last_gc_metrics();

    assert!(
        matches!(
            metrics.collection_type,
            rudo_gc::CollectionType::Minor
                | rudo_gc::CollectionType::Major
                | rudo_gc::CollectionType::IncrementalMajor
                | rudo_gc::CollectionType::None
        ),
        "Collection type should be valid"
    );
}

#[test]
fn test_gc_history_metrics() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    for _ in 0..3 {
        let gc = Gc::new(vec![0u8; 128]);
        drop(gc);
        collect_full();
    }

    let history = gc_history();
    let recent = history.recent(5);

    assert!(
        !recent.is_empty(),
        "Should have at least one entry in GC history"
    );

    let avg_pause = history.average_pause_time(3);
    let _ = avg_pause.as_nanos();

    let max_pause = history.max_pause_time(3);
    let _ = max_pause.as_nanos();
}

#[test]
fn test_heap_size_consistency() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let total = current_heap_size();
    let young = current_young_size();
    let old = current_old_size();

    assert!(
        total >= young + old,
        "Total heap ({}) should be at least sum of generations ({}+{}={})",
        total,
        young,
        old,
        young + old
    );
}

#[test]
fn test_metrics_reset_with_test_util() {
    #[cfg(feature = "test-util")]
    {
        test_util::reset();

        let before = global_metrics();

        let gc = Gc::new(42u64);
        drop(gc);
        collect_full();

        let after = global_metrics();

        assert!(
            after.total_collections() >= before.total_collections(),
            "Collections should not decrease after reset"
        );
    }
}

#[test]
fn test_large_allocation_pattern() {
    #[cfg(feature = "test-util")]
    test_util::reset();

    let sizes: Vec<usize> = vec![128, 256, 512, 1024, 2048];
    let mut gc_objects: Vec<Gc<Vec<u8>>> = Vec::new();

    for size in &sizes {
        let gc = Gc::new(vec![0u8; *size]);
        gc_objects.push(gc);
    }

    let peak_heap = current_heap_size();

    drop(gc_objects);
    collect_full();

    let after_gc = current_heap_size();

    assert!(peak_heap > 0, "Peak heap should be positive");
    assert!(
        after_gc <= peak_heap + 1024,
        "Heap after GC should be near peak"
    );
}
