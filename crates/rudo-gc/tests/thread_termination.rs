use rudo_gc::Gc;
use std::thread;

#[test]
fn test_many_threads_allocation_no_crash() {
    // Spawn many threads, each allocating some memory.
    // This verifies that LocalHeap::drop (and memory unmapping) doesn't crash.
    let mut handles = Vec::new();
    for _ in 0..50 {
        handles.push(thread::spawn(|| {
            for i in 0..100 {
                let _g = Gc::new(i);
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_large_object_thread_death() {
    #[derive(rudo_gc::Trace)]
    struct Big {
        _data: [u8; 5000],
    }

    let handle = thread::spawn(|| {
        let _g = Gc::new(Big { _data: [0; 5000] });
        // Thread exits, LocalHeap dropped, large object should be unmapped.
    });
    handle.join().unwrap();
}
