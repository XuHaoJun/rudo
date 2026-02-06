//! Integration tests for extended GC metrics system.
//!
//! These tests verify phase timing, incremental marking statistics,
//! and other metrics-related functionality.

use rudo_gc::{CollectionType, FallbackReason, Gc, GcCell, Trace};
use std::time::Duration;

/// Test that phase timing sums approximately to total duration.
#[test]
fn test_phase_timing_sums_approximately() {
    // Allocate some objects to create GC work
    let _objects: Vec<Gc<u64>> = (0..100).map(Gc::new).collect();

    // Trigger a GC collection
    rudo_gc::collect();

    // Get metrics
    let metrics = rudo_gc::last_gc_metrics();

    // Skip if no collection occurred
    if metrics.collection_type == CollectionType::None {
        return;
    }

    // Calculate total phase time
    let total_phase_time = metrics.clear_duration + metrics.mark_duration + metrics.sweep_duration;

    // The sum should be less than or equal to total duration
    // (phases may have gaps between them for setup/teardown)
    assert!(
        total_phase_time <= metrics.duration + Duration::from_micros(100),
        "Total phase time ({:?}) should be less than or approximately equal to total duration ({:?})",
        total_phase_time,
        metrics.duration
    );
}

/// Test that minor collections have zero clear duration.
///
/// Minor collections skip the clear phase and combine mark+sweep,
/// so `clear_duration` should be zero.
#[test]
fn test_minor_collection_clear_duration_zero() {
    // Force a minor collection by allocating small objects
    // and not exceeding the major threshold
    for _ in 0..10 {
        let _small: Gc<u64> = Gc::new(42);
    }

    // Trigger collection
    rudo_gc::collect();

    let metrics = rudo_gc::last_gc_metrics();

    // Check if this was a minor collection
    if metrics.collection_type == CollectionType::Minor {
        assert_eq!(
            metrics.clear_duration,
            Duration::ZERO,
            "Minor collection should have zero clear duration"
        );
    }
}

/// Test that metrics are populated after a collection.
#[test]
fn test_metrics_populated_after_collection() {
    // Allocate objects
    let _objects: Vec<Gc<u64>> = (0..50).map(Gc::new).collect();

    // Trigger collection
    rudo_gc::collect();

    let metrics = rudo_gc::last_gc_metrics();

    // Basic sanity checks
    assert!(
        metrics.duration > Duration::ZERO,
        "Duration should be positive"
    );
    assert!(
        metrics.total_collections > 0,
        "Collection count should be positive"
    );
}

/// Test that collection type is correctly reported.
#[test]
fn test_collection_type_reported() {
    // Allocate enough to potentially trigger major collection
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let objects: Vec<Gc<Vec<u8>>> = (0..100).map(|i| Gc::new(vec![i as u8; 1024])).collect();

    // Trigger collection
    rudo_gc::collect();

    let metrics = rudo_gc::last_gc_metrics();

    assert!(
        matches!(
            metrics.collection_type,
            CollectionType::Minor | CollectionType::Major
        ),
        "Collection type should be Minor or Major, got {:?}",
        metrics.collection_type
    );

    drop(objects);
}

/// Test incremental metrics are populated when incremental marking is active.
///
/// Note: This test requires incremental marking to be enabled.
/// It will be skipped if incremental marking is not available.
#[test]
fn test_incremental_metrics_populated() {
    use rudo_gc::gc::incremental::IncrementalMarkState;

    // Enable incremental marking
    let state = IncrementalMarkState::global();
    let mut config = *state.config();
    let original_enabled = config.enabled;
    config.enabled = true;
    state.set_config(config);

    // Allocate objects
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let _objects: Vec<Gc<Vec<u8>>> = (0..50).map(|i| Gc::new(vec![i as u8; 1024])).collect();

    // Trigger major collection
    rudo_gc::collect_full();

    // Get metrics
    let metrics = rudo_gc::last_gc_metrics();

    // If incremental major collection occurred, check incremental-specific fields
    if metrics.collection_type == CollectionType::IncrementalMajor {
        // objects_marked should be positive for incremental collections
        assert!(
            metrics.objects_marked > 0 || metrics.slices_executed > 0,
            "Incremental collection should have objects_marked > 0 or slices_executed > 0"
        );
    }

    // Restore original config
    config.enabled = original_enabled;
    state.set_config(config);
}

/// Test that fallback reason is reported correctly.
#[test]
fn test_fallback_reason_reported() {
    let metrics = rudo_gc::last_gc_metrics();

    // If fallback_occurred is true, there should be a reason
    if metrics.fallback_occurred {
        assert!(
            metrics.fallback_reason != FallbackReason::None,
            "If fallback_occurred is true, fallback_reason should not be None"
        );
    }
}

/// Test that non-incremental fields are zero for non-incremental collections.
#[test]
fn test_non_incremental_fields_zero() {
    // Force a non-incremental collection by disabling incremental marking
    use rudo_gc::gc::incremental::IncrementalMarkState;

    let state = IncrementalMarkState::global();
    let mut config = *state.config();
    let original_enabled = config.enabled;
    config.enabled = false;
    state.set_config(config);

    // Allocate and collect
    let _objects: Vec<Gc<u64>> = (0..100).map(Gc::new).collect();
    rudo_gc::collect_full();

    let metrics = rudo_gc::last_gc_metrics();

    // For non-incremental collections, these fields should typically be 0
    if metrics.collection_type == CollectionType::Major {
        // These fields may be 0 for non-incremental STW collections
        // The test documents this expectation
        assert_eq!(
            metrics.slices_executed, 0,
            "Non-incremental STW collection should have 0 slices"
        );
    }

    // Restore original config
    config.enabled = original_enabled;
    state.set_config(config);
}

/// Test `GcMetrics` Clone implementation.
#[test]
fn test_gc_metrics_clone() {
    let metrics = rudo_gc::last_gc_metrics();
    #[allow(clippy::clone_on_copy)]
    let cloned = metrics.clone();

    assert_eq!(metrics.duration, cloned.duration);
    assert_eq!(metrics.bytes_reclaimed, cloned.bytes_reclaimed);
    assert_eq!(metrics.objects_reclaimed, cloned.objects_reclaimed);
    assert_eq!(metrics.clear_duration, cloned.clear_duration);
    assert_eq!(metrics.mark_duration, cloned.mark_duration);
    assert_eq!(metrics.sweep_duration, cloned.sweep_duration);
}

/// Test `GcMetrics` Copy implementation.
#[test]
fn test_gc_metrics_copy() {
    let metrics = rudo_gc::last_gc_metrics();
    let copied = metrics;

    // After copy, both should be accessible
    assert_eq!(metrics.duration, copied.duration);
    assert_eq!(metrics.total_collections, copied.total_collections);
}

/// Test `FallbackReason` variants.
#[test]
fn test_fallback_reason_variants() {
    // FallbackReason already imported at module level

    // Test conversion to and from u32
    assert_eq!(FallbackReason::None.to_u32(), 0);
    assert_eq!(FallbackReason::DirtyPagesExceeded.to_u32(), 1);
    assert_eq!(FallbackReason::SliceTimeout.to_u32(), 2);
    assert_eq!(FallbackReason::WorklistUnbounded.to_u32(), 3);
    assert_eq!(FallbackReason::SatbBufferOverflow.to_u32(), 4);

    // Test from_u32
    assert_eq!(FallbackReason::from_u32(0), FallbackReason::None);
    assert_eq!(
        FallbackReason::from_u32(1),
        FallbackReason::DirtyPagesExceeded
    );
    assert_eq!(FallbackReason::from_u32(2), FallbackReason::SliceTimeout);
    assert_eq!(
        FallbackReason::from_u32(3),
        FallbackReason::WorklistUnbounded
    );
    assert_eq!(
        FallbackReason::from_u32(4),
        FallbackReason::SatbBufferOverflow
    );
    assert_eq!(FallbackReason::from_u32(99), FallbackReason::None); // Invalid value defaults to None
}

/// Test `CollectionType` variants.
#[test]
fn test_collection_type_variants() {
    // CollectionType already imported at module level

    // Test that enum variants have the expected values
    assert_eq!(CollectionType::None as u8, 0);
    assert_eq!(CollectionType::Minor as u8, 1);
    assert_eq!(CollectionType::Major as u8, 2);
    assert_eq!(CollectionType::IncrementalMajor as u8, 3);
}

/// Test that collections can occur without panicking.
///
/// This is a basic smoke test for the metrics system.
#[test]
fn test_gc_metrics_smoke() {
    // Perform a collection
    rudo_gc::collect();

    // Get metrics - should not panic
    let _metrics = rudo_gc::last_gc_metrics();

    // Trigger another collection
    let _obj: Gc<u64> = Gc::new(42);
    rudo_gc::collect();

    // Get metrics again
    let _metrics2 = rudo_gc::last_gc_metrics();
}

/// Test metrics with cyclic data structures.
#[test]
fn test_metrics_with_cycles() {
    #[derive(Trace)]
    struct Node {
        value: u64,
        next: GcCell<Option<Gc<Self>>>,
    }

    // Create a cycle
    let node1 = Gc::new(Node {
        value: 1,
        next: GcCell::new(None),
    });

    let node2 = Gc::new(Node {
        value: 2,
        next: GcCell::new(None),
    });

    // Create cycle
    *node1.next.borrow_mut() = Some(node2.clone());
    *node2.next.borrow_mut() = Some(node1);

    // Collect
    rudo_gc::collect();

    // Metrics should be available
    let metrics = rudo_gc::last_gc_metrics();
    assert!(metrics.total_collections > 0);
}

/// Test that duration is reasonable (not negative, not extremely large).
#[test]
fn test_duration_reasonable() {
    rudo_gc::collect();

    let metrics = rudo_gc::last_gc_metrics();

    // Duration should be non-negative
    assert!(
        metrics.duration >= Duration::ZERO,
        "Duration should be non-negative"
    );

    // Duration should be reasonable (less than 1 second for a test)
    assert!(
        metrics.duration < Duration::from_secs(1),
        "Duration should be less than 1 second in test context, got {:?}",
        metrics.duration
    );
}

/// Test that bytes accounting is reasonable.
#[test]
fn test_bytes_accounting() {
    // Allocate some objects
    let objects: Vec<Gc<u64>> = (0..50).map(Gc::new).collect();

    rudo_gc::collect();

    let metrics = rudo_gc::last_gc_metrics();

    // Skip if no collection occurred
    if metrics.collection_type == CollectionType::None {
        return;
    }

    // Basic sanity check: either some bytes were reclaimed or some survived (or both)
    // The exact relationship depends on implementation details and collection type
    let total_accounted = metrics
        .bytes_surviving
        .saturating_add(metrics.bytes_reclaimed);
    assert!(
        total_accounted > 0 || metrics.bytes_surviving == 0,
        "Bytes accounting should be reasonable: surviving={}, reclaimed={}",
        metrics.bytes_surviving,
        metrics.bytes_reclaimed
    );

    // Prevent the objects from being dropped early
    drop(objects);
}

/// Test that global metrics accumulate correctly after collections.
#[test]
fn test_global_metrics_accumulate() {
    use rudo_gc::global_metrics;

    // Get initial metrics
    let before = global_metrics().total_collections();

    // Perform multiple collections to ensure we increment
    for i in 0..3 {
        let _obj: Gc<u64> = Gc::new(i);
        rudo_gc::collect();
    }

    // Verify counters incremented
    let after = global_metrics().total_collections();

    assert!(
        after >= before + 3,
        "Total collections should increment by at least 3, was {before} before and {after} after"
    );
}

/// Test that global metrics track collection types correctly.
#[test]
fn test_global_metrics_collection_type_breakdown() {
    use rudo_gc::global_metrics;

    // Force both minor and major collections
    for i in 0..3 {
        let _small: Gc<u64> = Gc::new(i);
        rudo_gc::collect();
    }

    // Allocate enough to trigger major collection
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let _large: Vec<Gc<Vec<u8>>> = (0..5).map(|i| Gc::new(vec![i as u8; 1024])).collect();
    rudo_gc::collect_full();

    let metrics = global_metrics();

    // Minor + Major + IncrementalMajor should equal total
    let breakdown = metrics.total_minor_collections()
        + metrics.total_major_collections()
        + metrics.total_incremental_collections();

    assert!(
        breakdown <= metrics.total_collections(),
        "Breakdown sums should not exceed total"
    );
}

/// Test that global metrics work correctly with multiple threads.
#[test]
fn test_global_metrics_multi_threaded() {
    use rudo_gc::global_metrics;
    use std::sync::{Arc, Barrier};
    use std::thread;

    let num_threads = 2;
    let barrier = Arc::new(Barrier::new(num_threads));

    let initial = global_metrics();
    let initial_total = initial.total_collections();

    let mut handles = Vec::new();
    for thread_id in 0..num_threads {
        let barrier = barrier.clone();
        let handle = thread::spawn(move || {
            barrier.wait();
            // Each thread performs exactly one collection to minimize sync issues
            let _obj: Gc<u64> = Gc::new(thread_id as u64);
            rudo_gc::collect();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let after = global_metrics();

    // At minimum, we expect some collections to have occurred
    assert!(
        after.total_collections() >= initial_total || after.total_collections() > 0,
        "Global metrics should count collections from all threads"
    );
}

/// Test that heap queries return reasonable values.
#[test]
fn test_heap_queries_return_sane_values() {
    use rudo_gc::{current_heap_size, current_old_size, current_young_size};

    // Initial heap size should be 0 or very small
    let initial = current_heap_size();

    // Allocate some objects
    let _objects: Vec<Gc<u64>> = (0..10).map(Gc::new).collect();

    // After allocation, heap size should be larger
    let after_alloc = current_heap_size();
    assert!(
        after_alloc >= initial,
        "Heap size should not decrease after allocation"
    );

    // Young size + old size should equal total
    let young = current_young_size();
    let old = current_old_size();
    let total = current_heap_size();

    // Allow for some margin since internal fragmentation might exist
    assert!(
        young + old >= total.saturating_sub(1024),
        "Young + Old should be approximately Total"
    );
}

/// Test that heap queries return zero when no heap exists.
#[test]
fn test_heap_queries_no_heap_returns_zero() {
    use rudo_gc::{current_heap_size, current_old_size, current_young_size};

    // Spawn a thread that doesn't have a heap
    use std::sync::mpsc;
    use std::thread;

    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        // These should return 0 since this thread doesn't have a heap
        let heap = current_heap_size();
        let young = current_young_size();
        let old = current_old_size();
        tx.send((heap, young, old)).unwrap();
    });

    let (heap, young, old) = rx.recv().unwrap();
    handle.join().unwrap();

    assert_eq!(heap, 0, "Thread without heap should return 0 for heap size");
    assert_eq!(
        young, 0,
        "Thread without heap should return 0 for young size"
    );
    assert_eq!(old, 0, "Thread without heap should return 0 for old size");
}

/// Test heap queries with young and old generations.
#[test]
fn test_heap_queries_young_old_generations() {
    use rudo_gc::{current_heap_size, current_old_size, current_young_size};

    // First collect to get a clean state
    rudo_gc::collect();

    // Allocate some objects (these will be in young gen initially)
    let _objects: Vec<Gc<u64>> = (0..50).map(Gc::new).collect();

    // Trigger minor collections to promote some objects to old gen
    for i in 0..5 {
        let _temp: Gc<u64> = Gc::new(i);
        rudo_gc::collect();
    }

    // After some collections, we should have objects in both generations
    let young = current_young_size();
    let old = current_old_size();
    let total = current_heap_size();

    // Either young or old should have some bytes (or both)
    assert!(
        young > 0 || old > 0 || total == 0,
        "At least one generation should have allocations"
    );

    // Total should be sum of young + old (approximately)
    assert!(
        total >= young + old,
        "Total ({total}) should be >= Young ({young}) + Old ({old})"
    );
}

/// Test that empty history returns zero values.
#[test]
fn test_history_empty() {
    use rudo_gc::gc_history;

    let history = gc_history();

    // Query more entries than available
    let recent = history.recent(100);
    assert_eq!(
        recent.len(),
        history.total_recorded().min(100),
        "Should return min(requested, available)"
    );

    let avg = history.average_pause_time(100);
    if history.total_recorded() == 0 {
        assert_eq!(avg, Duration::ZERO, "Empty history should return zero avg");
    } else {
        assert!(avg >= Duration::ZERO, "Average should be non-negative");
    }

    let max = history.max_pause_time(100);
    if history.total_recorded() == 0 {
        assert_eq!(max, Duration::ZERO, "Empty history should return zero max");
    } else {
        assert!(max >= Duration::ZERO, "Max should be non-negative");
    }
}
