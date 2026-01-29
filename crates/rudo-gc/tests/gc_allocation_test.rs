use rudo_gc::{Gc, Trace};
use std::sync::atomic::AtomicBool;

#[derive(Trace)]
pub struct Simple {
    pub id: u64,
    pub flag: AtomicBool,
}

impl Simple {
    #[must_use]
    pub fn new(id: u64) -> Gc<Self> {
        Gc::new(Self {
            id,
            flag: AtomicBool::new(false),
        })
    }
}

#[test]
fn test_gc_allocation_unique() {
    let a = Simple::new(1);
    let b = Simple::new(2);
    let c = Simple::new(3);

    let addr_a = Gc::as_ptr(&a) as usize;
    let addr_b = Gc::as_ptr(&b) as usize;
    let addr_c = Gc::as_ptr(&c) as usize;

    println!("a = {addr_a:#x}");
    println!("b = {addr_b:#x}");
    println!("c = {addr_c:#x}");

    assert_ne!(addr_a, addr_b, "a and b should have different addresses");
    assert_ne!(addr_b, addr_c, "b and c should have different addresses");
    assert_ne!(addr_a, addr_c, "a and c should have different addresses");
}

#[test]
fn test_gc_allocation_sequence() {
    let objects: Vec<Gc<Simple>> = (0..100).map(Simple::new).collect();

    for (i, obj) in objects.iter().enumerate() {
        let addr = Gc::as_ptr(obj) as usize;
        println!("object[{i}] = {addr:#x}");
    }

    // Verify all addresses are unique
    let addrs: Vec<usize> = objects.iter().map(|o| Gc::as_ptr(o) as usize).collect();
    let unique_addrs: std::collections::HashSet<usize> = addrs.iter().copied().collect();
    assert_eq!(
        addrs.len(),
        unique_addrs.len(),
        "All addresses should be unique"
    );
}
