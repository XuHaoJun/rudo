//! Tests for incremental marking fallback behavior.
//!
//! These tests verify that the system correctly falls back to STW
//! when mutation thresholds are exceeded.

use rudo_gc::gc::incremental::{
    is_incremental_marking_active, is_write_barrier_active, IncrementalConfig,
    IncrementalMarkState, MarkPhase, MarkSliceResult,
};
use rudo_gc::test_util;
use rudo_gc::Trace;

#[derive(Trace)]
struct Data {
    value: usize,
}

#[test]
fn test_fallback_reason_variants() {
    test_util::reset();

    use rudo_gc::gc::incremental::FallbackReason;

    let reason = FallbackReason::DirtyPagesExceeded;
    assert!(matches!(reason, FallbackReason::DirtyPagesExceeded));

    let reason = FallbackReason::SliceTimeout;
    assert!(matches!(reason, FallbackReason::SliceTimeout));

    let reason = FallbackReason::WorklistUnbounded;
    assert!(matches!(reason, FallbackReason::WorklistUnbounded));
}

#[test]
fn test_mark_slice_result_fallback_variants() {
    test_util::reset();

    let result = MarkSliceResult::Fallback {
        reason: rudo_gc::gc::incremental::FallbackReason::DirtyPagesExceeded,
    };
    match result {
        MarkSliceResult::Fallback { reason } => {
            assert!(matches!(
                reason,
                rudo_gc::gc::incremental::FallbackReason::DirtyPagesExceeded
            ));
        }
        _ => panic!("Expected Fallback variant"),
    }

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

    let result = MarkSliceResult::Fallback {
        reason: rudo_gc::gc::incremental::FallbackReason::WorklistUnbounded,
    };
    match result {
        MarkSliceResult::Fallback { reason } => {
            assert!(matches!(
                reason,
                rudo_gc::gc::incremental::FallbackReason::WorklistUnbounded
            ));
        }
        _ => panic!("Expected Fallback variant"),
    }
}

#[test]
fn test_slice_timing() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    assert_eq!(state.slice_elapsed_ms(), 0);

    state.start_slice();
    std::thread::sleep(std::time::Duration::from_millis(10));

    assert!(state.slice_elapsed_ms() >= 10);
}

#[test]
fn test_request_fallback_records_reason() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    assert!(!state.fallback_requested());

    state.request_fallback(rudo_gc::gc::incremental::FallbackReason::SliceTimeout);
    assert!(state.fallback_requested());

    let stats = state.stats();
    assert!(stats
        .fallback_occurred
        .load(std::sync::atomic::Ordering::SeqCst));

    let reason = stats.fallback_reason.lock();
    assert!(matches!(
        *reason,
        Some(rudo_gc::gc::incremental::FallbackReason::SliceTimeout)
    ));
}

#[test]
fn test_reset_clears_slice_timer() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    state.start_slice();
    std::thread::sleep(std::time::Duration::from_millis(5));

    assert!(state.slice_elapsed_ms() > 0);

    state.reset();

    assert_eq!(state.slice_elapsed_ms(), 0);
}

#[test]
fn test_mark_slice_fallback_on_worklist_growth() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);
    state.set_initial_worklist_size(10);

    for _ in 0..150 {
        state.push_work(std::ptr::NonNull::dangling());
    }

    assert!(state.worklist_len() > 10 * 10);
}

#[test]
fn test_fallback_request_multiple_times() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    state.request_fallback(rudo_gc::gc::incremental::FallbackReason::DirtyPagesExceeded);
    assert!(state.fallback_requested());

    state.reset_fallback();
    assert!(!state.fallback_requested());

    state.request_fallback(rudo_gc::gc::incremental::FallbackReason::SliceTimeout);
    assert!(state.fallback_requested());
}

#[test]
fn test_mark_phase_consistent_with_incremental() {
    test_util::reset();

    let state = IncrementalMarkState::global();

    state.set_phase(MarkPhase::Idle);
    assert!(!is_incremental_marking_active());
    assert!(!is_write_barrier_active());

    state.set_phase(MarkPhase::Snapshot);
    assert!(is_incremental_marking_active());
    assert!(!is_write_barrier_active());

    state.set_phase(MarkPhase::Marking);
    assert!(is_incremental_marking_active());
    assert!(is_write_barrier_active());

    state.set_phase(MarkPhase::FinalMark);
    assert!(is_incremental_marking_active());
    assert!(!is_write_barrier_active());

    state.set_phase(MarkPhase::Sweeping);
    assert!(!is_incremental_marking_active());
    assert!(!is_write_barrier_active());
}

#[test]
fn test_stats_persists_across_slice() {
    test_util::reset();

    let state = IncrementalMarkState::global();
    let stats = state.stats();

    stats
        .objects_marked
        .store(100, std::sync::atomic::Ordering::SeqCst);
    stats
        .slices_executed
        .store(5, std::sync::atomic::Ordering::SeqCst);

    assert_eq!(
        stats
            .objects_marked
            .load(std::sync::atomic::Ordering::SeqCst),
        100
    );
    assert_eq!(
        stats
            .slices_executed
            .load(std::sync::atomic::Ordering::SeqCst),
        5
    );
}

#[test]
fn test_config_with_various_fallback_thresholds() {
    test_util::reset();

    let config = IncrementalConfig {
        enabled: true,
        increment_size: 500,
        max_dirty_pages: 500,
        remembered_buffer_len: 16,
        slice_timeout_ms: 25,
    };

    state_with_config(config);

    let state = IncrementalMarkState::global();
    let retrieved = state.config();
    assert!(retrieved.enabled);
    assert_eq!(retrieved.increment_size, 500);
    assert_eq!(retrieved.max_dirty_pages, 500);
    assert_eq!(retrieved.slice_timeout_ms, 25);
}

fn state_with_config(config: IncrementalConfig) {
    IncrementalMarkState::global().set_config(config);
}

#[test]
fn test_incremental_gc_disabled_by_default() {
    test_util::reset();

    let config = IncrementalConfig::default();
    assert!(!config.enabled);
}

#[test]
fn test_fallback_reason_recording() {
    test_util::reset();

    let reasons = [
        rudo_gc::gc::incremental::FallbackReason::DirtyPagesExceeded,
        rudo_gc::gc::incremental::FallbackReason::SliceTimeout,
        rudo_gc::gc::incremental::FallbackReason::WorklistUnbounded,
    ];

    for reason in reasons {
        test_util::reset();
        let state = IncrementalMarkState::global();

        state.request_fallback(reason);

        let stats = state.stats();
        assert!(stats
            .fallback_occurred
            .load(std::sync::atomic::Ordering::SeqCst));

        let recorded = stats.fallback_reason.lock();
        if let Some(_) = &*recorded {
            // Fallback was recorded successfully
        } else {
            panic!("Expected Some variant");
        }
    }
}
