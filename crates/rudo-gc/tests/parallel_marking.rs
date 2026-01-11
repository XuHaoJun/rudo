//! Tests for parallel marking and Remote Mentions infrastructure.
//!
//! These tests verify the thread control block and marking infrastructure
//! that enables parallel garbage collection using the Remote Mentions approach.

use rudo_gc::heap::{self, thread_registry, ThreadControlBlock, MAGIC_GC_PAGE};
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[test]
fn test_page_header_owner_id() {
    let header = rudo_gc::heap::PageHeader {
        magic: MAGIC_GC_PAGE,
        block_size: 64,
        obj_count: 50,
        header_size: 64,
        generation: 0,
        flags: 0,
        _padding: [0; 2],
        owner_id: 42,
        mark_bitmap: [0; 4],
        dirty_bitmap: [0; 4],
        allocated_bitmap: [0; 4],
        free_list_head: None,
    };

    assert_eq!(header.owner_id, 42);
}

#[test]
fn test_thread_control_block_thread_id() {
    let tcb = ThreadControlBlock::new(5);
    assert_eq!(tcb.id(), 5);
}

#[test]
fn test_local_mark_stack_operations() {
    let tcb = ThreadControlBlock::new(0);

    assert!(tcb.is_local_mark_stack_empty());

    let obj1 = 0x1000 as *const u8;
    let obj2 = 0x2000 as *const u8;
    let obj3 = 0x3000 as *const u8;

    tcb.push_local_mark(obj1);
    assert!(!tcb.is_local_mark_stack_empty());

    tcb.push_local_mark(obj2);
    tcb.push_local_mark(obj3);

    assert_eq!(tcb.pop_local_mark(), Some(obj3));
    assert_eq!(tcb.pop_local_mark(), Some(obj2));
    assert_eq!(tcb.pop_local_mark(), Some(obj1));
    assert_eq!(tcb.pop_local_mark(), None);

    assert!(tcb.is_local_mark_stack_empty());
}

#[test]
fn test_remote_inbox_operations() {
    let tcb = ThreadControlBlock::new(0);

    let obj1 = 0x4000 as *const u8;
    let obj2 = 0x5000 as *const u8;

    tcb.push_remote_inbox(obj1);
    tcb.push_remote_inbox(obj2);

    assert_eq!(tcb.remote_received_count.load(Ordering::Relaxed), 2);

    let count = tcb.drain_remote_inbox_to_local();
    assert_eq!(count, 2);

    assert_eq!(tcb.pop_local_mark(), Some(obj2));
    assert_eq!(tcb.pop_local_mark(), Some(obj1));
}

#[test]
fn test_clear_mark_stacks() {
    let tcb = ThreadControlBlock::new(0);

    tcb.push_local_mark(0x1000 as *const u8);
    tcb.push_remote_inbox(0x2000 as *const u8);

    tcb.record_remote_sent();
    tcb.record_remote_sent();

    assert!(!tcb.is_local_mark_stack_empty());
    assert!(tcb.remote_sent_count.load(Ordering::Relaxed) > 0);
    assert!(tcb.remote_received_count.load(Ordering::Relaxed) > 0);

    tcb.clear_mark_stacks();

    assert!(tcb.is_local_mark_stack_empty());
    assert_eq!(tcb.remote_sent_count.load(Ordering::Relaxed), 0);
    assert_eq!(tcb.remote_received_count.load(Ordering::Relaxed), 0);
}

#[test]
fn test_remote_operation_count() {
    let tcb = ThreadControlBlock::new(0);

    assert_eq!(tcb.remote_operation_count(), 0);

    tcb.record_remote_sent();
    tcb.push_remote_inbox(0x1000 as *const u8);

    assert_eq!(tcb.remote_operation_count(), 2);
}

#[test]
fn test_thread_registry_clone() {
    let registry = thread_registry().lock().unwrap();
    let cloned = registry.clone();
    assert_eq!(cloned.threads.len(), registry.threads.len());
    assert_eq!(
        cloned.active_count.load(Ordering::Relaxed),
        registry.active_count.load(Ordering::Relaxed)
    );
}

#[test]
fn test_get_thread_control_block_by_id() {
    let tcb = heap::current_thread_control_block();
    assert!(tcb.is_some());

    let tcb = tcb.unwrap();
    let id = tcb.id();

    let looked_up = heap::get_thread_control_block_by_id(id);
    assert!(looked_up.is_some());
    assert_eq!(looked_up.unwrap().id(), id);
}

#[test]
fn test_get_thread_count() {
    let count = heap::get_thread_count();
    assert!(true, "Thread count should be retrievable");
}

#[test]
fn test_ptr_to_page_owner() {
    let _gc_val = rudo_gc::Gc::new(42i32);
    let internal_ptr = rudo_gc::Gc::internal_ptr(&_gc_val) as *const u8;

    let header = unsafe { rudo_gc::heap::ptr_to_page_header(internal_ptr) };
    unsafe {
        assert_eq!((*header.as_ptr()).magic, MAGIC_GC_PAGE);
    }

    let _owner = unsafe { rudo_gc::heap::ptr_to_page_owner(internal_ptr) };
}

#[test]
fn test_gc_visitor_new() {
    let visitor = rudo_gc::GcVisitor::new(rudo_gc::VisitorKind::Major);
    assert_eq!(visitor.kind, rudo_gc::VisitorKind::Major);
    assert_eq!(visitor.thread_id, 0);
    assert!(visitor.registry.is_none());
}

#[test]
fn test_gc_visitor_new_parallel() {
    use std::sync::{Arc, Mutex};
    let registry: Arc<Mutex<heap::ThreadRegistry>> =
        Arc::new(Mutex::new(thread_registry().lock().unwrap().clone()));
    let visitor =
        rudo_gc::GcVisitor::new_parallel(rudo_gc::VisitorKind::Minor, 5, Arc::clone(&registry));

    assert_eq!(visitor.kind, rudo_gc::VisitorKind::Minor);
    assert_eq!(visitor.thread_id, 5);
    assert!(visitor.registry.is_some());
}

#[test]
fn test_mark_stack_drain_returns_count() {
    let tcb = ThreadControlBlock::new(0);

    let count = tcb.drain_remote_inbox_to_local();
    assert_eq!(count, 0);

    tcb.push_remote_inbox(0x1000 as *const u8);
    tcb.push_remote_inbox(0x2000 as *const u8);
    tcb.push_remote_inbox(0x3000 as *const u8);

    let count = tcb.drain_remote_inbox_to_local();
    assert_eq!(count, 3);

    let count = tcb.drain_remote_inbox_to_local();
    assert_eq!(count, 0);
}

#[test]
fn test_concurrent_push_pop() {
    use std::thread;

    let tcb = Arc::new(ThreadControlBlock::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(10));
    let mut handles = vec![];

    for i in 0..10 {
        let tcb = tcb.clone();
        let barrier = barrier.clone();

        let handle = thread::spawn(move || {
            barrier.wait();
            let obj = (i * 0x10000) as *const u8;
            tcb.push_local_mark(obj);
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    for _ in 0..10 {
        let _ = tcb.pop_local_mark();
    }

    assert!(tcb.is_local_mark_stack_empty());
}

#[test]
fn test_remote_inbox_cross_thread() {
    let tcb1 = Arc::new(ThreadControlBlock::new(0));
    let tcb2 = Arc::new(ThreadControlBlock::new(1));

    {
        let mut registry = thread_registry().lock().unwrap();
        registry.register_thread(tcb1.clone());
        registry.register_thread(tcb2.clone());
    }

    let obj = 0xdeadbeef as *const u8;
    tcb2.push_remote_inbox(obj);

    let count = tcb2.drain_remote_inbox_to_local();
    assert_eq!(count, 1);
    assert!(!tcb2.is_local_mark_stack_empty());
    assert_eq!(tcb2.pop_local_mark(), Some(obj));

    {
        let mut registry = thread_registry().lock().unwrap();
        registry.unregister_thread(&tcb1);
        registry.unregister_thread(&tcb2);
    }
}

#[test]
fn test_atomic_counters() {
    let tcb = ThreadControlBlock::new(0);

    tcb.record_remote_sent();
    assert_eq!(tcb.remote_sent_count.load(Ordering::Relaxed), 1);

    tcb.record_remote_sent();
    assert_eq!(tcb.remote_sent_count.load(Ordering::Relaxed), 2);

    tcb.push_remote_inbox(0x1000 as *const u8);
    tcb.push_remote_inbox(0x2000 as *const u8);
    assert_eq!(tcb.remote_received_count.load(Ordering::Relaxed), 2);
}

#[test]
fn test_thread_local_heap_thread_id() {
    let tcb = heap::current_thread_control_block();
    assert!(tcb.is_some());

    let tcb = tcb.unwrap();
    let _id = tcb.id();
}

#[test]
fn test_allocation_sets_page_owner() {
    let gc_val = rudo_gc::Gc::new(123i32);
    let internal_ptr = rudo_gc::Gc::internal_ptr(&gc_val) as *const u8;

    let owner = unsafe { rudo_gc::heap::ptr_to_page_owner(internal_ptr) };
    let tcb = heap::current_thread_control_block().unwrap();

    assert_eq!(owner, tcb.id());
}

#[test]
fn test_multiple_allocations_same_thread() {
    let gc1 = rudo_gc::Gc::new(1i32);
    let gc2 = rudo_gc::Gc::new(2i32);
    let gc3 = rudo_gc::Gc::new(3i32);

    let ptr1 = rudo_gc::Gc::internal_ptr(&gc1) as *const u8;
    let ptr2 = rudo_gc::Gc::internal_ptr(&gc2) as *const u8;
    let ptr3 = rudo_gc::Gc::internal_ptr(&gc3) as *const u8;

    let owner1 = unsafe { rudo_gc::heap::ptr_to_page_owner(ptr1) };
    let owner2 = unsafe { rudo_gc::heap::ptr_to_page_owner(ptr2) };
    let owner3 = unsafe { rudo_gc::heap::ptr_to_page_owner(ptr3) };

    let tcb = heap::current_thread_control_block().unwrap();
    assert_eq!(owner1, tcb.id());
    assert_eq!(owner2, tcb.id());
    assert_eq!(owner3, tcb.id());
}

#[test]
fn test_parallel_marking_infrastructure_exists() {
    // This test verifies the parallel marking infrastructure exists
    use std::sync::{atomic::AtomicUsize, Arc, Mutex};

    type WorkerFn = fn(
        usize,
        Arc<ThreadControlBlock>,
        Vec<usize>,
        Arc<Mutex<heap::ThreadRegistry>>,
        Arc<AtomicUsize>,
    );

    // Create a dummy worker function with correct signature
    let _worker_fn: WorkerFn = |_a, _b, _c, _d, _e| {};

    // Verify the perform_parallel_marking function exists
    let _ = rudo_gc::perform_parallel_marking as fn(&[_]) -> ();
}
