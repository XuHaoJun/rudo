//! Tests for incremental marking state machine transitions.

#![allow(clippy::significant_drop_tightening, clippy::items_after_statements)]

use rudo_gc::gc::incremental::{
    is_incremental_marking_active, is_write_barrier_active, IncrementalConfig,
    IncrementalMarkState, MarkPhase, MarkSliceResult,
};
use rudo_gc::test_util;

#[test]
fn test_state_machine_idle_to_snapshot() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    assert_eq!(state.phase(), MarkPhase::Idle);

    let result = state.transition_to(MarkPhase::Snapshot);
    assert!(result);
    assert_eq!(state.phase(), MarkPhase::Snapshot);
}

#[test]
fn test_state_machine_snapshot_to_marking() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);

    let result = state.transition_to(MarkPhase::Marking);
    assert!(result);
    assert_eq!(state.phase(), MarkPhase::Marking);
}

#[test]
fn test_state_machine_marking_to_final_mark() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let result = state.transition_to(MarkPhase::FinalMark);
    assert!(result);
    assert_eq!(state.phase(), MarkPhase::FinalMark);
}

#[test]
fn test_state_machine_final_mark_to_sweeping() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::FinalMark);

    let result = state.transition_to(MarkPhase::Sweeping);
    assert!(result);
    assert_eq!(state.phase(), MarkPhase::Sweeping);
}

#[test]
fn test_state_machine_sweeping_to_idle() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Sweeping);

    let result = state.transition_to(MarkPhase::Idle);
    assert!(result);
    assert_eq!(state.phase(), MarkPhase::Idle);
}

#[test]
fn test_state_machine_invalid_transitions() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    assert!(!state.transition_to(MarkPhase::Marking));
    assert_eq!(state.phase(), MarkPhase::Idle);

    state.set_phase(MarkPhase::Marking);
    assert!(!state.transition_to(MarkPhase::Idle));
    assert_eq!(state.phase(), MarkPhase::Marking);

    state.set_phase(MarkPhase::FinalMark);
    assert!(!state.transition_to(MarkPhase::Idle));
    assert_eq!(state.phase(), MarkPhase::FinalMark);
}

#[test]
fn test_is_incremental_marking_active() {
    test_util::reset();

    assert!(!is_incremental_marking_active());

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    assert!(is_incremental_marking_active());

    state.set_phase(MarkPhase::Marking);
    assert!(is_incremental_marking_active());

    state.set_phase(MarkPhase::FinalMark);
    assert!(is_incremental_marking_active());

    state.set_phase(MarkPhase::Idle);
    assert!(!is_incremental_marking_active());
}

#[test]
fn test_is_write_barrier_active() {
    test_util::reset();

    assert!(!is_write_barrier_active());

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);
    assert!(!is_write_barrier_active());

    state.set_phase(MarkPhase::Marking);
    assert!(is_write_barrier_active());

    state.set_phase(MarkPhase::FinalMark);
    assert!(!is_write_barrier_active());
}

#[test]
fn test_fallback_request() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    assert!(!state.fallback_requested());

    state.request_fallback(rudo_gc::gc::incremental::FallbackReason::DirtyPagesExceeded);
    assert!(state.fallback_requested());

    state.reset_fallback();
    assert!(!state.fallback_requested());
}

#[test]
fn test_worklist_operations() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    assert!(state.worklist_is_empty());

    use std::ptr::NonNull;
    let dummy_ptr = NonNull::dangling();

    state.push_work(dummy_ptr);
    assert!(!state.worklist_is_empty());

    let popped = state.pop_work();
    assert!(popped.is_some());
    assert!(state.worklist_is_empty());
}

#[test]
fn test_config_update() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    let config = state.config();
    assert!(!config.enabled);
    assert_eq!(config.increment_size, 1000);

    drop(config);

    let new_config = IncrementalConfig {
        enabled: true,
        increment_size: 500,
        max_dirty_pages: 500,
        remembered_buffer_len: 16,
        slice_timeout_ms: 25,
    };
    state.set_config(new_config);

    let config = state.config();
    assert!(config.enabled);
    assert_eq!(config.increment_size, 500);
    assert_eq!(config.max_dirty_pages, 500);
    assert_eq!(config.remembered_buffer_len, 16);
    assert_eq!(config.slice_timeout_ms, 25);
}

#[test]
fn test_mark_slice_result_pending() {
    test_util::reset();

    let result = MarkSliceResult::Pending {
        objects_marked: 100,
        dirty_pages_remaining: 50,
    };

    match result {
        MarkSliceResult::Pending {
            objects_marked,
            dirty_pages_remaining,
        } => {
            assert_eq!(objects_marked, 100);
            assert_eq!(dirty_pages_remaining, 50);
        }
        _ => panic!("Expected Pending variant"),
    }
}

#[test]
fn test_mark_slice_result_complete() {
    test_util::reset();

    let result = MarkSliceResult::Complete {
        total_objects_marked: 10000,
        total_slices: 10,
    };

    match result {
        MarkSliceResult::Complete {
            total_objects_marked,
            total_slices,
        } => {
            assert_eq!(total_objects_marked, 10000);
            assert_eq!(total_slices, 10);
        }
        _ => panic!("Expected Complete variant"),
    }
}

#[test]
fn test_mark_slice_result_fallback() {
    test_util::reset();

    let result = MarkSliceResult::Fallback {
        reason: rudo_gc::gc::incremental::FallbackReason::SliceTimeout,
    };

    match result {
        MarkSliceResult::Fallback { reason } => {
            assert!(matches!(
                reason,
                rudo_gc::gc::incremental::FallbackReason::SliceTimeout
            ));
        }
        _ => panic!("Expected Fallback variant"),
    }
}

#[test]
fn test_mark_phase_from_usize() {
    test_util::reset();

    assert_eq!(MarkPhase::from_usize(0), Some(MarkPhase::Idle));
    assert_eq!(MarkPhase::from_usize(1), Some(MarkPhase::Snapshot));
    assert_eq!(MarkPhase::from_usize(2), Some(MarkPhase::Marking));
    assert_eq!(MarkPhase::from_usize(3), Some(MarkPhase::FinalMark));
    assert_eq!(MarkPhase::from_usize(4), Some(MarkPhase::Sweeping));
    assert_eq!(MarkPhase::from_usize(5), None);
    assert_eq!(MarkPhase::from_usize(100), None);
}
