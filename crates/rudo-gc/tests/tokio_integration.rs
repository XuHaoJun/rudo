//! Integration tests for tokio async/await integration.

use rudo_gc::tokio::{spawn, GcRootGuard, GcRootSet, GcTokioExt};
use rudo_gc::{Gc, Trace};
use std::ptr::NonNull;

#[derive(Trace)]
struct TestData {
    value: i32,
}

fn register_test_root(ptr: NonNull<u8>) {
    GcRootSet::global().register(ptr.as_ptr() as usize);
}

#[test]
fn test_gcrootset_singleton() {
    let set1 = GcRootSet::global();
    let set2 = GcRootSet::global();
    assert!(std::ptr::eq(set1, set2));
}

#[test]
fn test_gcrootset_register_unregister() {
    let set = GcRootSet::global();
    set.clear();

    let initial_count = set.count();
    assert_eq!(initial_count, 0, "Initial count should be 0 after clear");

    let test_ptr = 0x1234 + set.count() + 1;
    set.register(test_ptr);
    assert_eq!(
        set.count(),
        initial_count + 1,
        "Count should increment after register"
    );
    assert!(set.is_registered(test_ptr), "Pointer should be registered");

    set.unregister(test_ptr);
    assert_eq!(
        set.count(),
        initial_count,
        "Count should return to initial after unregister"
    );
    assert!(
        !set.is_registered(test_ptr),
        "Pointer should not be registered"
    );
}

#[test]
fn test_gcrootset_snapshot() {
    let set = GcRootSet::global();
    set.clear();

    set.register(0x1000);
    set.register(0x2000);

    let snapshot = set.snapshot();
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.contains(&0x1000));
    assert!(snapshot.contains(&0x2000));
}

#[test]
fn test_guard_registration() {
    let gc = Gc::new(TestData { value: 42 });
    let ptr = gc_internal_ptr(&gc);

    register_test_root(ptr);

    let guard = unsafe { GcRootGuard::new(ptr) };
    assert!(GcRootSet::global().is_registered(ptr.as_ptr() as usize));

    drop(guard);
    assert!(!GcRootSet::global().is_registered(ptr.as_ptr() as usize));
}

#[test]
fn test_guard_unregistration_only_once() {
    let gc = Gc::new(TestData { value: 42 });
    let ptr = gc_internal_ptr(&gc);

    register_test_root(ptr);
    let guard = unsafe { GcRootGuard::new(ptr) };
    drop(guard);

    assert!(!GcRootSet::global().is_registered(ptr.as_ptr() as usize));
}

#[test]
fn test_root_guard_creation() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let gc = Gc::new(TestData { value: 42 });
        let _guard = gc.root_guard();
        assert_eq!(gc.value, 42);
    });
}

#[test]
fn test_yield_now() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let gc = Gc::new(TestData { value: 42 });
        gc.yield_now().await;
        assert_eq!(gc.value, 42);
    });
}

#[test]
fn test_spawn_basic() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let gc = Gc::new(TestData { value: 42 });
        let ptr = gc_internal_ptr(&gc);

        register_test_root(ptr);

        let _guard = unsafe { GcRootGuard::new(ptr) };

        let result = spawn(async move { gc.value }).await;
        assert_eq!(result, 42);
    });
}

#[test]
fn test_multiple_spawns() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let gc = Gc::new(TestData { value: 42 });
        let ptr = gc_internal_ptr(&gc);
        register_test_root(ptr);
        let _guard = unsafe { GcRootGuard::new(ptr) };

        let handles: Vec<_> = (0..5)
            .map(|i| {
                let gc = gc.clone();
                spawn(async move { gc.value + i })
            })
            .collect();

        let mut results = Vec::new();
        for h in handles {
            results.push(h.await);
        }

        assert_eq!(results, vec![42, 43, 44, 45, 46]);
    });
}

fn gc_internal_ptr<T: Trace + 'static>(gc: &Gc<T>) -> NonNull<u8> {
    let ptr = Gc::<T>::as_ptr(gc);
    NonNull::new(ptr as *mut u8).unwrap()
}
