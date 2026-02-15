//! Tests for Weak<T> behavior during incremental marking phases.
//!
//! These tests verify that Weak::upgrade(), is_alive(), and may_be_valid()
//! work correctly throughout the different phases of incremental marking:
//! Idle, Snapshot, Marking, FinalMark, and Sweeping.

#![allow(clippy::redundant_closure)]

use rudo_gc::gc::incremental::{IncrementalConfig, IncrementalMarkState, MarkPhase};
use rudo_gc::{collect_full, set_incremental_config, Gc, Trace, Weak};

#[cfg(feature = "test-util")]
use rudo_gc::test_util::{clear_test_roots, internal_ptr, register_test_root};

#[cfg(feature = "test-util")]
macro_rules! root {
    ($gc:expr) => {
        register_test_root(internal_ptr(&$gc))
    };
}

#[cfg(not(feature = "test-util"))]
macro_rules! root {
    ($gc:expr) => {};
}

#[cfg(feature = "test-util")]
macro_rules! clear_roots {
    () => {
        clear_test_roots()
    };
}

#[cfg(not(feature = "test-util"))]
macro_rules! clear_roots {
    () => {};
}

fn enable_incremental() {
    let config = IncrementalConfig {
        enabled: true,
        increment_size: 100,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    };
    set_incremental_config(config);
}

fn disable_incremental() {
    let config = IncrementalConfig {
        enabled: false,
        increment_size: 100,
        max_dirty_pages: 1000,
        remembered_buffer_len: 32,
        slice_timeout_ms: 50,
    };
    set_incremental_config(config);
}

fn reset_to_idle() {
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Idle);
}

#[derive(Trace)]
struct TestData {
    value: i32,
}

// ============================================================================
// Test 1: Weak upgrade in different mark phases
// ============================================================================

#[test]
fn test_weak_upgrade_in_idle_phase() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Idle);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    assert!(weak.upgrade().is_some());

    reset_to_idle();
    disable_incremental();
}

#[test]
fn test_weak_upgrade_in_snapshot_phase() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Snapshot);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    assert!(weak.upgrade().is_some());

    reset_to_idle();
    disable_incremental();
}

#[test]
fn test_weak_upgrade_in_marking_phase() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    assert!(weak.upgrade().is_some());

    reset_to_idle();
    disable_incremental();
}

#[test]
fn test_weak_upgrade_in_final_mark_phase() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::FinalMark);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    assert!(weak.upgrade().is_some());

    reset_to_idle();
    disable_incremental();
}

#[test]
fn test_weak_upgrade_in_sweeping_phase() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Sweeping);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    // In sweeping phase, the object should still be upgradeable
    // because we haven't actually collected yet
    assert!(weak.upgrade().is_some());

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 2: Weak is_alive and may_be_valid in different phases
// ============================================================================

#[test]
fn test_weak_is_alive_across_all_phases() {
    enable_incremental();
    let state = IncrementalMarkState::global();

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    let phases = [
        MarkPhase::Idle,
        MarkPhase::Snapshot,
        MarkPhase::Marking,
        MarkPhase::FinalMark,
        MarkPhase::Sweeping,
    ];

    for phase in phases {
        state.set_phase(phase);
        assert!(weak.is_alive(), "Weak should be alive in {:?} phase", phase);
    }

    reset_to_idle();
    disable_incremental();
}

#[test]
fn test_weak_may_be_valid_across_all_phases() {
    enable_incremental();
    let state = IncrementalMarkState::global();

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    let phases = [
        MarkPhase::Idle,
        MarkPhase::Snapshot,
        MarkPhase::Marking,
        MarkPhase::FinalMark,
        MarkPhase::Sweeping,
    ];

    for phase in phases {
        state.set_phase(phase);
        assert!(
            weak.may_be_valid(),
            "Weak may_be_valid in {:?} phase",
            phase
        );
    }

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 3: Weak survives full incremental collection cycle
// ============================================================================

#[test]
fn test_weak_survives_full_incremental_cycle() {
    enable_incremental();
    let state = IncrementalMarkState::global();

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    // Run through full cycle
    state.set_phase(MarkPhase::Snapshot);
    assert!(weak.upgrade().is_some());

    state.set_phase(MarkPhase::Marking);
    assert!(weak.upgrade().is_some());

    state.set_phase(MarkPhase::FinalMark);
    assert!(weak.upgrade().is_some());

    state.set_phase(MarkPhase::Sweeping);
    assert!(weak.upgrade().is_some());

    state.set_phase(MarkPhase::Idle);
    assert!(weak.upgrade().is_some());

    disable_incremental();
}

#[test]
fn test_weak_after_full_collect_cycle() {
    clear_roots!();

    enable_incremental();
    let state = IncrementalMarkState::global();

    let gc = Gc::new(TestData { value: 42 });
    root!(gc);
    let weak = Gc::downgrade(&gc);

    // Complete cycle
    state.set_phase(MarkPhase::Snapshot);
    state.set_phase(MarkPhase::Marking);
    state.set_phase(MarkPhase::FinalMark);
    state.set_phase(MarkPhase::Sweeping);
    state.set_phase(MarkPhase::Idle);

    // Weak should still work while gc is alive
    assert!(weak.upgrade().is_some());

    // Drop gc and collect
    drop(gc);
    clear_roots!();
    collect_full();

    // After collection, weak should be dead
    assert!(weak.upgrade().is_none());

    reset_to_idle();
    disable_incremental();
    clear_roots!();
}

// ============================================================================
// Test 4: Weak with multiple strong refs during marking
// ============================================================================

#[test]
fn test_weak_with_multiple_strong_refs_during_marking() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let gc = Gc::new(TestData { value: 42 });
    let gc2 = Gc::clone(&gc);
    let weak = Gc::downgrade(&gc);

    // Both strong refs exist
    assert!(weak.upgrade().is_some());

    // Drop first strong ref
    drop(gc);
    assert!(weak.upgrade().is_some());

    // Drop second strong ref
    drop(gc2);

    // No strong refs, but in marking phase - upgrade may succeed
    // because actual collection hasn't happened yet
    let _result = weak.upgrade();
    // Result depends on implementation - the object is not yet swept
    // but may not be upgradeable if ref_count is 0

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 5: try_upgrade during marking phases
// ============================================================================

#[test]
fn test_try_upgrade_in_marking_phase() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    assert!(weak.try_upgrade().is_some());

    reset_to_idle();
    disable_incremental();
}

#[test]
fn test_try_upgrade_in_sweeping_phase() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Sweeping);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    // try_upgrade should work the same as upgrade
    assert!(weak.try_upgrade().is_some());

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 6: Weak ptr_eq during marking phases
// ============================================================================

#[test]
fn test_weak_ptr_eq_during_marking() {
    enable_incremental();
    let state = IncrementalMarkState::global();

    let gc = Gc::new(TestData { value: 42 });
    let weak1 = Gc::downgrade(&gc);
    let weak2 = Gc::downgrade(&gc);

    let phases = [
        MarkPhase::Idle,
        MarkPhase::Snapshot,
        MarkPhase::Marking,
        MarkPhase::FinalMark,
        MarkPhase::Sweeping,
    ];

    for phase in phases {
        state.set_phase(phase);
        assert!(
            Weak::ptr_eq(&weak1, &weak2),
            "ptr_eq should work in {:?} phase",
            phase
        );
    }

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 7: Weak clone during marking phases
// ============================================================================

#[test]
fn test_weak_clone_during_marking() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let gc = Gc::new(TestData { value: 42 });
    let weak1 = Gc::downgrade(&gc);
    let weak2 = weak1.clone();

    assert!(weak1.upgrade().is_some());
    assert!(weak2.upgrade().is_some());
    assert!(Weak::ptr_eq(&weak1, &weak2));

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 8: Weak counts during marking phases
// ============================================================================

#[test]
fn test_weak_counts_during_marking() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let gc = Gc::new(TestData { value: 42 });
    assert_eq!(Gc::weak_count(&gc), 0);

    let weak1 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 1);

    let _weak2 = Gc::downgrade(&gc);
    assert_eq!(Gc::weak_count(&gc), 2);

    let _weak3 = weak1.clone();
    assert_eq!(Gc::weak_count(&gc), 3);

    assert_eq!(weak1.strong_count(), 1);
    assert_eq!(weak1.weak_count(), 3);

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 9: Weak in GcCell during marking
// ============================================================================

#[test]
fn test_weak_in_gccell_during_marking() {
    use rudo_gc::cell::GcCell;

    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
        value: i32,
    }

    let node = Gc::new_cyclic_weak(|weak| Node {
        self_ref: GcCell::new(Some(weak)),
        value: 42,
    });

    // Access weak self ref during marking
    let weak_self = node.self_ref.borrow();
    let upgraded = weak_self.as_ref().unwrap().upgrade().unwrap();
    assert_eq!(upgraded.value, 42);
    assert!(Gc::ptr_eq(&node, &upgraded));

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 10: Weak upgrade after gc drops during marking
// ============================================================================

#[test]
fn test_weak_after_gc_drop_during_marking() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let gc = Gc::new(TestData { value: 42 });
    let weak = Gc::downgrade(&gc);

    // Drop gc while in marking phase
    drop(gc);

    // In marking phase with no strong refs, upgrade should return None
    // because ref_count is 0
    assert!(weak.upgrade().is_none());

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 11: Multiple weaks to same object across phases
// ============================================================================

#[test]
fn test_many_weaks_across_phases() {
    enable_incremental();
    let state = IncrementalMarkState::global();

    let gc = Gc::new(TestData { value: 42 });
    let weak_refs: Vec<Weak<TestData>> = (0..100).map(|_| Gc::downgrade(&gc)).collect();

    let phases = [
        MarkPhase::Idle,
        MarkPhase::Snapshot,
        MarkPhase::Marking,
        MarkPhase::FinalMark,
        MarkPhase::Sweeping,
    ];

    for phase in phases {
        state.set_phase(phase);
        for weak in &weak_refs {
            assert!(
                weak.upgrade().is_some(),
                "All weaks should upgrade in {:?} phase",
                phase
            );
        }
    }

    reset_to_idle();
    disable_incremental();
}

// ============================================================================
// Test 12: Weak default during marking
// ============================================================================

#[test]
fn test_weak_default_during_marking() {
    enable_incremental();
    let state = IncrementalMarkState::global();
    state.set_phase(MarkPhase::Marking);

    let weak: Weak<TestData> = Weak::default();

    assert!(!weak.is_alive());
    assert!(!weak.may_be_valid());
    assert!(weak.upgrade().is_none());
    assert!(weak.try_upgrade().is_none());
    assert_eq!(weak.strong_count(), 0);
    assert_eq!(weak.weak_count(), 0);

    reset_to_idle();
    disable_incremental();
}
