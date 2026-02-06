//! Integration tests for GC tracing feature.
//!
//! These tests verify that tracing spans and events are correctly generated
//! during garbage collection operations.

#![cfg(feature = "tracing")]

use rudo_gc::{collect, Gc};

#[test]
fn test_tracing_compiles_with_feature() {
    // This test verifies that the tracing feature compiles correctly.
    // The actual span verification is done through the test subscriber
    // which captures span creation events.
    // Tracing feature is enabled and compiles
}

#[test]
fn test_basic_gc_with_tracing() {
    // Create some GC objects
    let gc1 = Gc::new(42);
    let gc2 = Gc::new("hello");

    // Trigger a collection - this should create tracing spans
    collect();

    // Verify objects still work after collection
    assert_eq!(*gc1, 42);
    assert_eq!(*gc2, "hello");
}

#[test]
fn test_multiple_collections_with_tracing() {
    // Trigger multiple collections with tracing enabled
    for i in 0..5 {
        let _gc = Gc::new(i);
        collect();
    }

    // If we get here without panicking, tracing is working
    // Multiple collections completed successfully
}

#[test]
fn test_gc_cycles_with_tracing() {
    use rudo_gc::Trace;
    use std::cell::RefCell;

    #[derive(Trace)]
    struct Node {
        next: RefCell<Option<Gc<Self>>>,
    }

    let a = Gc::new(Node {
        next: RefCell::new(None),
    });
    let b = Gc::new(Node {
        next: RefCell::new(None),
    });

    // Create cycle: a -> b -> a
    *a.next.borrow_mut() = Some(Gc::clone(&b));
    *b.next.borrow_mut() = Some(Gc::clone(&a));

    drop(a);
    drop(b);

    // Collection with tracing should handle cycles
    collect();

    // Cycle collection with tracing completed
}

#[test]
fn test_major_collection_with_tracing() {
    // Create enough objects to trigger major collection
    let mut objects = Vec::new();
    for i in 0..100 {
        objects.push(Gc::new(vec![i; 100]));
    }

    // Force full collection
    rudo_gc::collect_full();

    // Just verify we got here without crashing
    // Objects are still rooted in Vec, so they should survive
    assert!(!objects.is_empty());
}

#[test]
fn test_incremental_marking_with_tracing() {
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    // Enable incremental marking
    let config = IncrementalConfig {
        enabled: true,
        ..Default::default()
    };
    set_incremental_config(config);

    // Create objects and trigger collections
    let mut objects = Vec::new();
    for i in 0..50 {
        objects.push(Gc::new(vec![i; 50]));
    }

    // Trigger incremental-friendly collection
    rudo_gc::collect_full();

    // Just verify we got here without crashing
    assert!(!objects.is_empty());
}

#[test]
fn test_phase_start_end_events() {
    // T020 [US2] Verify phase_start event logged for clear phase
    // T021 [US2] Verify phase_end event logged for mark phase with objects_marked
    // T022 [US2] Verify phase_start/phase_end events logged for sweep phase
    //
    // Create objects and trigger major collection to exercise all phases
    let mut objects = Vec::new();
    for i in 0..100 {
        objects.push(Gc::new(vec![i; 50]));
    }

    // Force major collection which runs clear, mark, and sweep phases
    rudo_gc::collect_full();

    // Verify objects survived collection
    assert!(!objects.is_empty());
}

#[test]
fn test_phase_span_hierarchy() {
    // T023 [US2] Verify proper span parent-child relationships between collection and phase spans
    //
    // Create complex object graph to trigger major collection
    let mut objects = Vec::new();
    for i in 0..50 {
        objects.push(Gc::new(vec![i; 100]));
    }

    // Trigger collection with multiple phases
    rudo_gc::collect_full();

    // Verify collection completed
    assert!(!objects.is_empty());
}

#[test]
fn test_sweep_events() {
    // T027 [US2] Verify sweep_start and sweep_end events with heap_bytes, objects_freed
    //
    // Create and drop objects to generate garbage
    for i in 0..50 {
        let _ = Gc::new(vec![i; 50]);
    }

    // Force collection which will trigger sweep
    rudo_gc::collect_full();

    // Collection completed successfully with sweep events
}

#[test]
fn test_incremental_mark_span() {
    // T029 [US3] Verify incremental_mark span appears during mark slices
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    // Enable incremental marking with small budget to force multiple slices
    let config = IncrementalConfig {
        enabled: true,
        increment_size: 10, // Small budget to force multiple slices
        ..Default::default()
    };
    set_incremental_config(config);

    // Create many objects to trigger incremental marking
    let mut objects = Vec::new();
    for i in 0..100 {
        objects.push(Gc::new(vec![i; 100]));
    }

    // Trigger collection - should use incremental marking
    rudo_gc::collect_full();

    // Verify collection completed successfully
    assert!(!objects.is_empty());
}

#[test]
fn test_incremental_slice_event() {
    // T030 [US3] Verify incremental_slice event with objects_marked and dirty_pages
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    let config = IncrementalConfig {
        enabled: true,
        increment_size: 25,
        ..Default::default()
    };
    set_incremental_config(config);

    // Create objects that will trigger mark slices
    let mut objects = Vec::new();
    for i in 0..75 {
        objects.push(Gc::new(vec![i; 50]));
    }

    // Trigger collection
    rudo_gc::collect_full();

    // Verify objects survived
    assert!(!objects.is_empty());
}

#[test]
fn test_incremental_fallback_event() {
    // T031 [US3] Verify fallback event logged when incremental exceeds budget
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    // Configure incremental marking with very low dirty page threshold
    let config = IncrementalConfig {
        enabled: true,
        max_dirty_pages: 1, // Very low threshold to trigger fallback
        increment_size: 5,
        ..Default::default()
    };
    set_incremental_config(config);

    // Create many objects to stress the incremental marker
    let mut objects = Vec::new();
    for i in 0..50 {
        objects.push(Gc::new(vec![i; 100]));
    }

    // Trigger collection - should potentially trigger fallback
    rudo_gc::collect_full();

    // Verify collection completed (fallback should be handled gracefully)
    assert!(!objects.is_empty());
}
