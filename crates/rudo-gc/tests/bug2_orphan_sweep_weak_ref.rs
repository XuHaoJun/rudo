//! Test for Bug 2: Orphan Sweep Unmaps Weak-Referenced Objects
//!
//! This test verifies that weak references to orphaned objects keep the
//! memory mapped even after the value is dropped. Calling `upgrade()` should
//! return None (not segfault).

use rudo_gc::{collect_full, Gc, Trace, Weak};
use std::thread;

#[repr(C)]
#[allow(clippy::large_stack_arrays)]
struct LargeStruct {
    data: [u64; 10000],
}

unsafe impl Trace for LargeStruct {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

#[test]
#[allow(clippy::large_stack_arrays)]
fn test_weak_ref_survives_orphan_sweep() {
    let weak_ref: Weak<LargeStruct>;

    let handle = thread::spawn(|| {
        Gc::downgrade(&Gc::new(LargeStruct {
            data: [0xCC; 10000],
        }))
    });

    weak_ref = handle.join().unwrap();

    collect_full();

    let result = std::panic::catch_unwind(|| weak_ref.upgrade());

    assert!(
        result.is_ok(),
        "upgrade() should not panic - memory should be mapped"
    );
    let upgraded = result.unwrap();
    assert!(
        upgraded.is_none(),
        "Upgrade should return None (value is dead)"
    );

    drop(weak_ref);
    collect_full();
}
