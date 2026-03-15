//! Regression test for bug 248: `GcScope::spawn` missing object liveness validation.
//!
//! When a tracked object is collected before `spawn()`, `spawn` must panic rather than
//! create a handle to freed or reused memory.

#![cfg(feature = "tokio")]

use rudo_gc::handles::GcScope;
use rudo_gc::{collect_full, Gc, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_gcscope_spawn_panics_when_tracked_object_collected() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async {
            rudo_gc::test_util::reset();
            let mut scope = GcScope::new();

            // Track an object
            let gc = Gc::new(Data { value: 42 });
            scope.track(&gc);

            // Drop the only strong reference so the object becomes collectible
            drop(gc);

            // Force GC to collect and sweep the object
            collect_full();

            // Spawn should panic: tracked object was deallocated
            let _: i32 = scope.spawn(|_handles| async move { 0i32 }).await;
        });
    }));

    assert!(
        result.is_err(),
        "GcScope::spawn should panic when tracked object was collected"
    );
}
