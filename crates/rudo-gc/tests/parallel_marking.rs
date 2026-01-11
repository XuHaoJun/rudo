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
        padding: [0; 2],
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
    drop(registry);
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
    let _count = heap::get_thread_count();
}

#[test]
fn test_ptr_to_page_owner() {
    let gc_val = rudo_gc::Gc::new(42i32);
    let internal_ptr = rudo_gc::Gc::internal_ptr(&gc_val);

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

    let obj = 0xdead_beef as *const u8;
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
    let internal_ptr = rudo_gc::Gc::internal_ptr(&gc_val);

    let owner = unsafe { rudo_gc::heap::ptr_to_page_owner(internal_ptr) };
    let tcb = heap::current_thread_control_block().unwrap();

    assert_eq!(owner, tcb.id());
}

#[test]
fn test_multiple_allocations_same_thread() {
    let gc1 = rudo_gc::Gc::new(1i32);
    let gc2 = rudo_gc::Gc::new(2i32);
    let gc3 = rudo_gc::Gc::new(3i32);

    let ptr1 = rudo_gc::Gc::internal_ptr(&gc1);
    let ptr2 = rudo_gc::Gc::internal_ptr(&gc2);
    let ptr3 = rudo_gc::Gc::internal_ptr(&gc3);

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
    let worker_fn: WorkerFn = |_a, _b, _c, _d, _e| {};
    let _ = worker_fn;

    // Verify the perform_parallel_marking function exists
    let _ = rudo_gc::perform_parallel_marking as fn(&[_]) -> ();
}

#[cfg(feature = "parallel-gc")]
mod parallel_gc_tests {
    use super::*;
    #[cfg(feature = "test-util")]
    use rudo_gc::test_util::clear_test_roots;

    #[cfg(not(feature = "test-util"))]
    fn clear_test_roots() {}

    use rudo_gc::{Gc, Trace};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    #[derive(Trace)]
    struct SharedNode {
        next: Option<Gc<Self>>,
        value: i32,
    }

    // Unsafe wrapper to send Gc between threads for testing purposes
    struct SendGc<T: Trace + 'static>(Gc<T>);
    #[allow(clippy::non_send_fields_in_send_ty)]
    unsafe impl<T: Trace + 'static> Send for SendGc<T> {}
    unsafe impl<T: Trace + 'static> Sync for SendGc<T> {}

    #[test]
    fn test_parallel_gc_with_multiple_threads() {
        clear_test_roots();

        let num_threads = 4;
        let barrier = Arc::new(Barrier::new(num_threads));
        let mut handles = Vec::new();

        for _ in 0..num_threads {
            let barrier = barrier.clone();
            let handle = thread::spawn(move || {
                barrier.wait();

                for i in 0..100 {
                    let _gc_val = rudo_gc::Gc::new(i);
                }

                rudo_gc::safepoint();
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        clear_test_roots();
    }

    #[test]
    #[cfg(not(miri))]
    fn test_parallel_gc_termination_and_redistribution() {
        clear_test_roots();

        // This test aims to trigger "remote marking" where Thread A marks an object
        // owned by Thread B, putting it into Thread B's Remote Inbox.

        let num_threads = 2;
        let barrier = Arc::new(Barrier::new(num_threads));
        let shared_link = Arc::new(Mutex::new(None));

        let b1 = barrier.clone();
        let s1 = shared_link.clone();
        let handle1 = thread::spawn(move || {
            // Thread 1 allocates a chain of nodes
            let node1 = Gc::new(SharedNode {
                next: None,
                value: 10,
            });
            let node2 = Gc::new(SharedNode {
                next: Some(node1),
                value: 20,
            });
            let weak1 = Gc::downgrade(&node2);

            // Share the head with Thread 2
            *s1.lock().unwrap() = Some(SendGc(node2));

            b1.wait(); // Sync 1: Shared

            rudo_gc::collect_full();

            assert!(weak1.upgrade().is_some());
        });

        let handle2 = thread::spawn(move || {
            barrier.wait(); // Sync 1: Wait for Thread 1 to share

            let SendGc(node_from_t1) = shared_link.lock().unwrap().take().unwrap();
            let weak2 = Gc::downgrade(&node_from_t1);

            rudo_gc::collect_full();

            assert!(weak2.upgrade().is_some());

            // Keep alive for Thread 1
            drop(node_from_t1);
        });

        handle1.join().unwrap();
        handle2.join().unwrap();

        clear_test_roots();
    }

    #[test]
    #[cfg(not(miri))]
    fn test_parallel_gc_termination() {
        clear_test_roots();

        let num_threads = 2;
        let barrier = Arc::new(Barrier::new(num_threads));

        let mut handles = Vec::new();

        for _ in 0..num_threads {
            let barrier = barrier.clone();
            let handle = thread::spawn(move || {
                barrier.wait();

                for i in 0..50 {
                    let val = rudo_gc::Gc::new(i);
                    drop(val);
                }

                rudo_gc::collect_full();
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        clear_test_roots();
    }

    #[test]
    fn test_parallel_gc_worker_count() {
        let count = heap::get_worker_count();
        assert!(count >= 1, "Worker count should be at least 1");
    }

    #[test]
    fn test_send_buffer_operations() {
        let tcb = ThreadControlBlock::new(0);

        let ptr1 = 0x1000 as *const u8;
        let ptr2 = 0x2000 as *const u8;

        tcb.queue_remote_forward(ptr1, 0);
        tcb.queue_remote_forward(ptr2, 0);

        tcb.flush_send_buffer();

        let buffer = unsafe { &*tcb.send_buffer.get() };
        assert!(buffer.is_empty(), "Send buffer should be empty after flush");
    }

    /// Test cross-thread reference cycles.
    ///
    /// This test verifies that the parallel GC correctly handles objects that form
    /// references across thread boundaries:
    /// - Thread A allocates node_a
    /// - Thread B allocates node_b
    /// - Thread A creates a chain: chain_a -> node_b (cross-thread reference)
    /// - Thread B creates a chain: chain_b -> node_a (cross-thread reference)
    /// - After GC, all reachable nodes should survive
    #[test]
    #[cfg(not(miri))]
    fn test_parallel_marking_cross_thread_references() {
        clear_test_roots();

        let barrier_share = Arc::new(Barrier::new(2));
        let barrier_link = Arc::new(Barrier::new(2));
        let barrier_gc = Arc::new(Barrier::new(2));

        // Storage for initial nodes - only written once, read once
        let node_from_a: Arc<Mutex<Option<SendGc<SharedNode>>>> = Arc::new(Mutex::new(None));
        let node_from_b: Arc<Mutex<Option<SendGc<SharedNode>>>> = Arc::new(Mutex::new(None));

        let bs_a = barrier_share.clone();
        let bl_a = barrier_link.clone();
        let bg_a = barrier_gc.clone();
        let nfa = node_from_a.clone();
        let nfb = node_from_b.clone();

        // Thread A
        let handle_a = thread::spawn(move || {
            // Allocate node_a on Thread A's heap
            let node_a = Gc::new(SharedNode {
                next: None,
                value: 100,
            });
            let weak_a = Gc::downgrade(&node_a);

            // Share node_a with Thread B
            *nfa.lock().unwrap() = Some(SendGc(Gc::clone(&node_a)));

            bs_a.wait(); // Sync: Both initial nodes shared

            // Get node_b from Thread B (allocated on Thread B's heap)
            let node_b = {
                let guard = nfb.lock().unwrap();
                Gc::clone(&guard.as_ref().unwrap().0)
            };
            let weak_b = Gc::downgrade(&node_b);

            // Create a chain on Thread A that references node_b (cross-thread)
            let chain_a = Gc::new(SharedNode {
                next: Some(Gc::clone(&node_b)),
                value: 101,
            });
            let weak_chain_a = Gc::downgrade(&chain_a);

            bl_a.wait(); // Sync: Cross-thread references established

            // Trigger GC
            rudo_gc::collect_full();

            bg_a.wait(); // Sync: GC complete

            // Verify all nodes survived
            assert!(weak_a.upgrade().is_some(), "node_a should survive");
            assert!(weak_b.upgrade().is_some(), "node_b should survive");
            assert!(weak_chain_a.upgrade().is_some(), "chain_a should survive");

            // Verify cross-thread reference is valid
            assert!(chain_a.next.is_some());
            assert_eq!(chain_a.next.as_ref().unwrap().value, 200);

            // Keep nodes alive
            drop(node_a);
            drop(node_b);
            drop(chain_a);
        });

        let bs_b = barrier_share;
        let bl_b = barrier_link;
        let bg_b = barrier_gc;
        let nfa2 = node_from_a;
        let nfb2 = node_from_b;

        // Thread B
        let handle_b = thread::spawn(move || {
            // Allocate node_b on Thread B's heap
            let node_b = Gc::new(SharedNode {
                next: None,
                value: 200,
            });
            let weak_b = Gc::downgrade(&node_b);

            // Share node_b with Thread A
            *nfb2.lock().unwrap() = Some(SendGc(Gc::clone(&node_b)));

            bs_b.wait(); // Sync: Both initial nodes shared

            // Get node_a from Thread A (allocated on Thread A's heap)
            let node_a = {
                let guard = nfa2.lock().unwrap();
                Gc::clone(&guard.as_ref().unwrap().0)
            };
            let weak_a = Gc::downgrade(&node_a);

            // Create a chain on Thread B that references node_a (cross-thread)
            let chain_b = Gc::new(SharedNode {
                next: Some(Gc::clone(&node_a)),
                value: 201,
            });
            let weak_chain_b = Gc::downgrade(&chain_b);

            bl_b.wait(); // Sync: Cross-thread references established

            // Trigger GC
            rudo_gc::collect_full();

            bg_b.wait(); // Sync: GC complete

            // Verify all nodes survived
            assert!(weak_b.upgrade().is_some(), "node_b should survive");
            assert!(weak_a.upgrade().is_some(), "node_a should survive");
            assert!(weak_chain_b.upgrade().is_some(), "chain_b should survive");

            // Verify cross-thread reference is valid
            assert!(chain_b.next.is_some());
            assert_eq!(chain_b.next.as_ref().unwrap().value, 100);

            // Keep nodes alive
            drop(node_b);
            drop(node_a);
            drop(chain_b);
        });

        handle_a.join().expect("Thread A panicked");
        handle_b.join().expect("Thread B panicked");

        clear_test_roots();
    }
}
