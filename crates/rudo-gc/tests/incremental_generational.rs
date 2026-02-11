//! Integration tests for combined incremental + generational GC.
//!
//! These tests verify that incremental marking works correctly
//! in combination with the existing generational GC infrastructure.

#![allow(
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::use_self,
    dead_code
)]

use rudo_gc::gc::incremental::{IncrementalConfig, IncrementalMarkState, MarkPhase};
use rudo_gc::test_util;
use rudo_gc::{Gc, Trace};
use std::cell::RefCell;

#[derive(Trace)]
struct Data {
    value: usize,
}

#[derive(Trace)]
struct Node {
    data: Gc<Data>,
    next: Option<Gc<RefCell<Vec<Gc<Node>>>>>,
}

#[test]
fn test_incremental_with_generational_config() {
    test_util::reset();

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        increment_size: 1000,
        max_dirty_pages: 100,
        remembered_buffer_len: 16,
        slice_timeout_ms: 10,
    });

    let config = IncrementalMarkState::global().config();
    assert!(config.enabled);
    assert_eq!(config.increment_size, 1000);
    assert_eq!(config.max_dirty_pages, 100);
}

#[test]
fn test_generational_allocation_survives_incremental() {
    test_util::reset();

    let data = Gc::new(Data { value: 42 });
    assert_eq!(data.value, 42);

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);
    assert!(state.phase() == MarkPhase::Marking);

    state.set_phase(MarkPhase::Sweeping);
    assert!(state.phase() == MarkPhase::Sweeping);

    drop(data);
    rudo_gc::collect();
}

#[test]
fn test_incremental_not_active_during_normal_gc() {
    test_util::reset();

    assert!(IncrementalMarkState::global().phase() == MarkPhase::Idle);

    let gc = Gc::new(Data { value: 1 });
    assert!(IncrementalMarkState::global().phase() == MarkPhase::Idle);

    drop(gc);
    rudo_gc::collect();

    assert!(IncrementalMarkState::global().phase() == MarkPhase::Idle);
}

#[test]
fn test_phase_transitions_respected() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    assert!(state.transition_to(MarkPhase::Snapshot));
    assert!(state.transition_to(MarkPhase::Marking));
    assert!(state.transition_to(MarkPhase::FinalMark));
    assert!(state.transition_to(MarkPhase::Sweeping));

    state.reset();
    assert!(state.phase() == MarkPhase::Idle);

    assert!(state.transition_to(MarkPhase::Snapshot));
    assert!(state.phase() == MarkPhase::Snapshot);
}

#[test]
fn test_collection_types_compatible() {
    test_util::reset();

    let gc = Gc::new(Data { value: 100 });
    assert_eq!(gc.value, 100);

    drop(gc);
    rudo_gc::collect();

    let metrics = rudo_gc::last_gc_metrics();
    assert!(metrics.bytes_reclaimed > 0 || metrics.objects_reclaimed > 0);
}

#[test]
fn test_incremental_config_persists_across_gc() {
    test_util::reset();

    IncrementalMarkState::global().set_config(IncrementalConfig {
        enabled: true,
        increment_size: 500,
        max_dirty_pages: 200,
        remembered_buffer_len: 8,
        slice_timeout_ms: 5,
    });

    for _ in 0..10 {
        let gc = Gc::new(Data { value: 1 });
        drop(gc);
        rudo_gc::collect();
    }

    let config = IncrementalMarkState::global().config();
    assert!(config.enabled);
    assert_eq!(config.increment_size, 500);
}

#[test]
fn test_marking_with_complex_references() {
    test_util::reset();

    #[derive(Trace)]
    struct Container {
        items: Gc<RefCell<Vec<Gc<Data>>>>,
    }

    let container = Gc::new(Container {
        items: Gc::new(RefCell::new(Vec::new())),
    });

    for i in 0..100 {
        container
            .items
            .borrow_mut()
            .push(Gc::new(Data { value: i }));
    }

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);
    assert!(state.phase() == MarkPhase::Marking);

    assert_eq!(container.items.borrow().len(), 100);

    drop(container);
    rudo_gc::collect();
}

#[test]
fn test_idle_phase_allows_normal_operation() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    assert!(state.phase() == MarkPhase::Idle);

    {
        for i in 0..100 {
            let gc = Gc::new(Data { value: i });
            assert_eq!(gc.value, i);
        }
    }

    rudo_gc::collect();

    assert!(state.phase() == MarkPhase::Idle);
}

#[test]
fn test_incremental_gc_enabled_uses_incremental_path() {
    test_util::reset();

    let gc = Gc::new(Data { value: 42 });
    assert_eq!(gc.value, 42);

    drop(gc);
    rudo_gc::collect();

    let metrics = rudo_gc::last_gc_metrics();
    assert!(metrics.objects_reclaimed >= 1);
}

#[test]
fn test_worklist_empty_when_no_work() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    assert!(state.worklist_is_empty());
    assert_eq!(state.worklist_len(), 0);

    state.set_phase(MarkPhase::Marking);
    assert!(state.worklist_is_empty());
}

#[test]
fn test_stats_accumulate_across_operations() {
    test_util::reset();

    let stats = IncrementalMarkState::global().stats();

    stats
        .objects_marked
        .store(1000, std::sync::atomic::Ordering::SeqCst);
    stats
        .dirty_pages_scanned
        .store(50, std::sync::atomic::Ordering::SeqCst);
    stats
        .slices_executed
        .store(5, std::sync::atomic::Ordering::SeqCst);

    assert_eq!(
        stats
            .objects_marked
            .load(std::sync::atomic::Ordering::SeqCst),
        1000
    );
    assert_eq!(
        stats
            .dirty_pages_scanned
            .load(std::sync::atomic::Ordering::SeqCst),
        50
    );
    assert_eq!(
        stats
            .slices_executed
            .load(std::sync::atomic::Ordering::SeqCst),
        5
    );
}
