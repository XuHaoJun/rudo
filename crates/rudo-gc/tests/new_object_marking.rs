use rudo_gc::test_util::reset;
use rudo_gc::{collect, Gc};

#[derive(rudo_gc::Trace)]
struct TestData {
    value: usize,
}

#[test]
fn test_new_object_survives_gc_default_mode() {
    reset();

    assert!(!rudo_gc::gc::incremental::is_incremental_marking_active());

    let gc = Gc::new(TestData { value: 42 });
    assert_eq!(gc.value, 42);

    collect();

    assert_eq!(gc.value, 42);
}

#[test]
fn test_multiple_new_objects_survive_gc() {
    reset();

    let objects: Vec<Gc<TestData>> = (0..100).map(|i| Gc::new(TestData { value: i })).collect();

    collect();

    for (i, obj) in objects.iter().enumerate() {
        assert_eq!(obj.value, i, "Object {i} was incorrectly collected");
    }
}

#[test]
fn test_object_survives_before_gc_runs() {
    reset();

    let gc = Gc::new(TestData { value: 123 });
    let ptr = Gc::as_ptr(&gc) as usize;

    collect();

    let gc_ref = unsafe { &*(ptr as *const TestData) };
    assert_eq!(gc_ref.value, 123);
}

#[test]
fn test_nested_objects_survive_gc() {
    reset();

    let inner = Gc::new(TestData { value: 999 });
    let outer = Gc::new(NestedData {
        inner: inner.clone(),
        value: 555,
    });

    collect();

    assert_eq!(inner.value, 999);
    assert_eq!(outer.value, 555);
    assert_eq!(outer.inner.value, 999);
}

#[derive(rudo_gc::Trace)]
struct NestedData {
    inner: Gc<TestData>,
    value: usize,
}
