//! Test if Vec capacity pre-allocation fixes the corruption
//!
//! Run: cargo test --test `vec_gc_corruption_minimal` -- --test-threads=1

use rudo_gc::gc::register_test_root_region;
use rudo_gc::Gc;
use rudo_gc::Trace;
use std::cell::RefCell;

#[derive(Trace)]
pub struct Component {
    pub id: u64,
}

impl Component {
    #[must_use]
    pub fn new(id: u64) -> Gc<Self> {
        Gc::new(Self { id })
    }
}

/// Test with Vec capacity pre-allocated
#[test]
#[cfg_attr(miri, ignore)]
fn test_preallocated_vec() {
    let items: RefCell<Vec<Gc<Component>>> = RefCell::new(Vec::with_capacity(1000));

    // Register the Vec's buffer as a region to be conservatively scanned for Gc pointers
    {
        let items_ref = items.borrow();
        let ptr = items_ref.as_ptr().cast::<u8>();
        let len = items_ref.capacity() * std::mem::size_of::<Gc<Component>>();
        eprintln!(
            "TEST: Registering Vec buffer ptr={:#x}, len={len}",
            ptr as usize
        );
        register_test_root_region(ptr, len);
    }

    for round in 0..15 {
        for i in 0..20 {
            let id = u64::try_from(round * 20 + i).unwrap();
            let component = Component::new(id);
            items.borrow_mut().push(Gc::clone(&component));
        }

        // Re-register the root region after pushes (Vec might reallocate)
        {
            let items_ref = items.borrow();
            let ptr = items_ref.as_ptr().cast::<u8>();
            let len = items_ref.capacity() * std::mem::size_of::<Gc<Component>>();
            if round % 5 == 4 {
                eprintln!(
                    "TEST: Round {round}: Before GC, Vec buffer ptr={:#x}, len={len}",
                    ptr as usize
                );
            }
            register_test_root_region(ptr, len);
        }

        if round % 5 == 4 {
            rudo_gc::collect();
        }

        for (idx, comp) in items.borrow().iter().enumerate() {
            let expected = u64::try_from(idx).unwrap();
            assert!(
                comp.id == expected,
                "Round {round}: idx {idx} corrupted! Expected {expected}, got {}",
                comp.id
            );
        }
    }

    rudo_gc::collect();
}

/// Test WITHOUT any GC during creation - should definitely pass
#[test]
#[cfg_attr(miri, ignore)]
fn test_no_gc_at_all() {
    let items: RefCell<Vec<Gc<Component>>> = RefCell::new(Vec::new());

    for round in 0..15 {
        for i in 0..20 {
            let id = u64::try_from(round * 20 + i).unwrap();
            let component = Component::new(id);
            items.borrow_mut().push(Gc::clone(&component));
        }

        // NO GC during creation
    }

    // GC only at the end
    rudo_gc::collect();

    for (idx, comp) in items.borrow().iter().enumerate() {
        let expected = u64::try_from(idx).unwrap();
        assert_eq!(comp.id, expected, "Final: idx {idx} corrupted");
    }

    rudo_gc::collect();
}

/// Test checking pointers before and after GC
#[test]
#[cfg_attr(miri, ignore)]
fn test_pointer_stability() {
    let items: RefCell<Vec<Gc<Component>>> = RefCell::new(Vec::with_capacity(100));

    // Create some components
    for i in 0..50 {
        let id = u64::try_from(i).unwrap();
        let component = Component::new(id);
        let ptr = Gc::as_ptr(&component) as usize;
        items.borrow_mut().push(Gc::clone(&component));

        eprintln!("Created id={id}, ptr={ptr:#x}");
    }

    // Register the Vec's buffer
    {
        let items_ref = items.borrow();
        register_test_root_region(
            items_ref.as_ptr().cast::<u8>(),
            items_ref.capacity() * std::mem::size_of::<Gc<Component>>(),
        );
    }

    // Check pointers
    for (idx, comp) in items.borrow().iter().enumerate() {
        let ptr = Gc::as_ptr(comp) as usize;
        eprintln!("Before GC: idx={idx}, id={}, ptr={ptr:#x}", comp.id);
    }

    rudo_gc::collect();

    // Check pointers again
    for (idx, comp) in items.borrow().iter().enumerate() {
        let ptr = Gc::as_ptr(comp) as usize;
        eprintln!("After GC: idx={idx}, id={}, ptr={ptr:#x}", comp.id);
        assert_eq!(comp.id, u64::try_from(idx).unwrap(), "idx {idx} corrupted");
    }
}
