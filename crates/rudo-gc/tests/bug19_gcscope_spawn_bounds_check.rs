//! Regression test for bug 19: `GcScope::spawn` missing bounds check causes buffer overflow.
//!
//! When tracking more than 256 objects, `GcScope::spawn` must panic rather than overflow.

#![cfg(feature = "tokio")]

use rudo_gc::handles::GcScope;
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[test]
fn test_gcscope_spawn_exceeds_handle_limit_panics() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async {
            rudo_gc::test_util::reset();
            let mut scope = GcScope::new();
            let objects: Vec<Gc<Data>> = (0..257).map(|i| Gc::new(Data { value: i })).collect();
            scope.track_slice(&objects);
            let _: i32 = scope.spawn(|_handles| async move { 0i32 }).await;
        });
    }));

    assert!(
        result.is_err(),
        "`GcScope::spawn` should panic when exceeding 256 handles"
    );
}
