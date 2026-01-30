use rudo_gc::{Gc, Trace, Weak};
use std::panic;

#[test]
#[allow(clippy::redundant_clone)]
fn test_reproduce_zst_cache_uaf() {
    // 1. Create a ZST Gc.
    let unit = Gc::new(());

    // 2. Drop it to trigger collection.
    drop(unit);

    // 3. Collect. This should sweep the ZST/() allocation because it's unreachable.
    // The thread-local ZST_BOX still holds a pointer to it, but ZST_BOX is not a root.
    rudo_gc::collect_full();

    // 4. Create a new ZST Gc.
    // This will hit the ZST_BOX cache, which points to the swept memory.
    // Tlab allocator might have reused that memory for something else (e.g. a different small object).
    // Or it's just plain UAF.
    let unit2 = Gc::new(());

    // 5. Accessing unit2 might crash or read garbage if memory was reused.
    // Since it's (), deref is a no-op, but 'inc_ref' inside clone would write to freed memory.
    let _ = unit2.clone();
}

#[test]
#[allow(dead_code, unreachable_code, dependency_on_unit_never_type_fallback)]
fn test_reproduce_panic_safety_failure() {
    // 1. Define a type that panics on trace or construction if we want,
    // but here we want to panic inside new_cyclic_weak's closure.

    #[derive(Trace)]
    #[allow(dead_code)]
    struct Bomb;

    // 2. Use catch_unwind to survive the panic.
    // Note: Gc assumes single threaded, so this is fine.

    let result = panic::catch_unwind(|| {
        Gc::new_cyclic_weak(|_weak| -> () {
            // Leak the weak pointer to the outside world
            // In a real exploit, this could be a thread-local or global.
            // Here we can just demonstrate logic.
            // But we need to access it AFTER the panic.
            // Since we can't easily stash it in a local variable outside the closure
            // (borrow checker), we use a RefCell or similar if we could.
            // But 'weak' is owned. We can clone it if we had a place to put it.

            // For repro, let's just panic.
            // The DropGuard checks 'completed'. It will be false.
            // destructors run.
            panic!("Boom");
        })
    });

    assert!(result.is_err());

    // If we had managed to exfiltrate the Weak, we could show UAF.
    // But since `new_cyclic_weak` API passes an owned `Weak`,
    // the user code inside the closure *owns* it.
    // If the closure panics, the `Weak` is dropped as stack unwinds.
    // So the `Weak` inside the closure is destroyed.
    // UNLESS the user moved it to a long-lived location (RefCell, global).
}

#[derive(Trace)]
#[allow(clippy::use_self)]
struct Stash {
    w: std::cell::RefCell<Option<Weak<Self>>>,
}

thread_local! {
    static STASH: std::cell::RefCell<Option<Weak<Stash>>> = const { std::cell::RefCell::new(None) };
}

#[test]
fn test_reproduce_panic_uaf() {
    let result = panic::catch_unwind(|| {
        Gc::new_cyclic_weak(|weak| {
            // Stash the weak pointer globally
            STASH.with(|stash| *stash.borrow_mut() = Some(weak.clone()));

            panic!("Boom"); // Trigger cleanup
        })
    });

    assert!(result.is_err());

    // Now access the stashed weak. logic suggests it points to deallocated memory.
    STASH.with(|stash| {
        if let Some(weak) = &*stash.borrow() {
            // upgrade() accesses the ref count in the allocation.
            // If allocation is freed, this is UAF.
            // Miri should catch this.
            // In normal execution, it might segfault or read garbage.
            let _ = weak.upgrade();
        }
    });
}
