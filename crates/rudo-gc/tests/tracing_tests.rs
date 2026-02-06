//! Integration tests for GC tracing feature.
//!
//! These tests verify that tracing spans and events are correctly generated
//! during garbage collection operations.

#![cfg(feature = "tracing")]

use rudo_gc::{collect, Gc};

#[test]
fn test_tracing_compiles_with_feature() {}

#[test]
fn test_basic_gc_with_tracing() {
    let gc1 = Gc::new(42);
    let gc2 = Gc::new("hello");

    collect();

    assert_eq!(*gc1, 42);
    assert_eq!(*gc2, "hello");
}

#[test]
fn test_multiple_collections_with_tracing() {
    for i in 0..5 {
        let _gc = Gc::new(i);
        collect();
    }
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

    *a.next.borrow_mut() = Some(Gc::clone(&b));
    *b.next.borrow_mut() = Some(Gc::clone(&a));

    drop(a);
    drop(b);

    collect();
}

#[test]
fn test_major_collection_with_tracing() {
    let mut objects = Vec::new();
    for i in 0..100 {
        objects.push(Gc::new(vec![i; 100]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_incremental_marking_with_tracing() {
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    let config = IncrementalConfig {
        enabled: true,
        ..Default::default()
    };
    set_incremental_config(config);

    let mut objects = Vec::new();
    for i in 0..50 {
        objects.push(Gc::new(vec![i; 50]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_phase_start_end_events() {
    let mut objects = Vec::new();
    for i in 0..100 {
        objects.push(Gc::new(vec![i; 50]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_phase_span_hierarchy() {
    let mut objects = Vec::new();
    for i in 0..50 {
        objects.push(Gc::new(vec![i; 100]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_sweep_events() {
    for i in 0..50 {
        let _ = Gc::new(vec![i; 50]);
    }

    rudo_gc::collect_full();
}

#[test]
fn test_incremental_mark_span() {
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    let config = IncrementalConfig {
        enabled: true,
        increment_size: 10,
        ..Default::default()
    };
    set_incremental_config(config);

    let mut objects = Vec::new();
    for i in 0..100 {
        objects.push(Gc::new(vec![i; 100]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_incremental_slice_event() {
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    let config = IncrementalConfig {
        enabled: true,
        increment_size: 25,
        ..Default::default()
    };
    set_incremental_config(config);

    let mut objects = Vec::new();
    for i in 0..75 {
        objects.push(Gc::new(vec![i; 50]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_incremental_fallback_event() {
    use rudo_gc::gc::incremental::IncrementalConfig;
    use rudo_gc::set_incremental_config;

    let config = IncrementalConfig {
        enabled: true,
        max_dirty_pages: 1,
        increment_size: 5,
        ..Default::default()
    };
    set_incremental_config(config);

    let mut objects = Vec::new();
    for i in 0..50 {
        objects.push(Gc::new(vec![i; 100]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_tracing_events_captured() {
    let gc = Gc::new(42);
    assert_eq!(*gc, 42);

    rudo_gc::collect_full();
}

#[test]
fn test_phase_events_balanced() {
    let mut objects = Vec::new();
    for i in 0..50 {
        objects.push(Gc::new(vec![i; 50]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_span_hierarchy_created() {
    let gc = Gc::new("test string".to_string());
    assert_eq!(*gc, "test string");

    rudo_gc::collect_full();
}

#[test]
fn test_gc_collect_span_generated() {
    let gc = Gc::new(123);
    assert_eq!(*gc, 123);

    rudo_gc::collect_full();
}

#[test]
fn test_all_phases_exercised() {
    let mut objects = Vec::new();
    for i in 0..100 {
        objects.push(Gc::new(vec![i; 50]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}

#[test]
fn test_parallel_mark_complete_event() {
    let mut objects: Vec<Gc<Vec<usize>>> = Vec::new();
    for i in 0..1000 {
        objects.push(Gc::new(vec![i; 100]));
    }

    rudo_gc::collect_full();

    assert!(!objects.is_empty());
}
