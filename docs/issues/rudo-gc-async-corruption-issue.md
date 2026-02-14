# rudo-gc Issue: Thread Safety Violation - Writing Gc-Managed Signals from Tokio Worker Threads

## Summary

Writing to Gc-managed signals from tokio worker threads causes heap corruption ("corrupted double-linked list"). The issue is that `AsyncHandleScope` can only track Gc objects allocated on the SAME thread where the scope is created, but tokio's multi-threaded runtime spawns tasks to different worker threads.

## Environment

- rudo-gc version: 0.8.10
- Rust version: 1.84+
- Platform: Linux (x86_64)
- Tokio: multi-threaded runtime

## Updated Minimal Reproduce Test

This test more accurately reproduces the issue - it shows that the crash occurs when writing to Gc-managed data from a worker thread:

```rust
// Save as: crates/rudo-gc/tests/async_signal_write_corruption.rs

use rudo_gc::handles::AsyncHandleScope;
use rudo_gc::heap::current_thread_control_block;
use rudo_gc::{Gc, GcCell, Trace};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::cell::RefCell;

#[derive(Trace, Clone)]
struct SignalData {
    value: GcCell<i32>,
}

impl SignalData {
    fn new(value: i32) -> Self {
        Self { value: GcCell::new(value) }
    }
    
    fn set(&self, new_value: i32) {
        // This is the problematic operation - writing to Gc-managed data
        // from a worker thread
        *self.value.borrow_mut_gen_only() = new_value;
    }
}

#[test]
fn test_async_signal_write_from_worker_thread() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // Initialize GC on main thread
        rudo_gc::test_util::reset();
        let _tcb = current_thread_control_block().expect("should have TCB");
        
        // Create a Gc-managed signal on main thread
        let signal: Gc<SignalData> = Gc::new(SignalData::new(0));
        
        // Spawn async task to worker thread
        let signal_clone = signal.clone();
        let handle = rt.spawn(async move {
            // This task runs on a worker thread, NOT the main thread
            let tcb = current_thread_control_block().expect("Async task requires GC thread");
            let scope = AsyncHandleScope::new(&tcb);
            
            // Track the signal
            let _handle = scope.handle(&signal_clone);
            
            // Try to write to the signal - THIS CAUSES CORRUPTION
            signal_clone.set(42);
            
            println!("Write completed");
        });
        
        // Wait for task to complete
        rt.block_on(handle).unwrap();
        
        println!("Test completed - if you see this, no corruption");
    });
}
```

## Expected Behavior

Signal writes from async tasks should either:
1. Work correctly with proper thread-safe GC operations
2. Or fail with a clear error message indicating thread safety violation

## Actual Behavior

Program crashes with:
- "corrupted double-linked list"
- "IOT instruction (core dumped)"
- Sometimes "misaligned pointer dereference"

The crash happens inside `GcCell::borrow_mut_gen_only()` or similar operations when called from a worker thread.

## Root Cause Analysis

### The Fundamental Issue

1. **Tokio multi-threaded runtime uses worker threads**: When you use `Builder::new_multi_thread()`, tokio spawns tasks to a pool of worker threads, not the main thread.

2. **Gc heap is thread-local**: The rudo-gc heap is created on the main thread. Worker threads have their own TLS, but they don't have access to the main thread's GC heap.

3. **AsyncHandleScope doesn't solve this**: `AsyncHandleScope` allows tracking Gc roots across await points, but it doesn't change the fundamental issue that Gc objects are allocated on a specific thread's heap.

4. **Writing from worker threads corrupts heap**: When a worker thread tries to write to Gc-managed data (via `borrow_mut_gen_only`), it's modifying memory in the main thread's heap from a different thread, causing corruption.

### Why the Original Test Didn't Catch This

The original minimal test created NEW Gc objects inside the async task, which might work (if the worker thread has its own heap or if Gc allocation works across threads). The real issue is writing to EXISTING Gc objects that were created on the main thread.

### Evidence from rvue

In rvue's hackernews example:
1. User clicks "refresh" button
2. Resource effect runs, spawns async task
3. Async task completes on worker thread
4. Task tries to write to resource state signal (Gc-managed)
5. **Crash: "corrupted double-linked list"**

## Related Code

- `crates/rvue/src/async_runtime/resource.rs` - create_resource that spawns async tasks
- `crates/rudo-gc/src/handles/async.rs` - AsyncHandleScope implementation
- `crates/rudo-gc/src/cell/gc_cell.rs` - GcCell with borrow_mut_gen_only

## Workarounds Attempted

### 1. Using GcRootGuard

`GcRootGuard` registers GC roots at process level, working across threads, but doesn't solve the fundamental issue of Gc allocation/borrowing on worker threads.

### 2. spawnUsing `tok_main_thread

io::task::spawn_local` or custom main thread spawner to run Gc operations on the main thread. This works but requires:
- All Gc operations in async tasks to be dispatched to main thread
- Using a channel/dispatcher to send closures to main thread
- Cannot use standard tokio spawn for anything involving Gc

### 3. Single-threaded Tokio

Using `Builder::new_current_thread()` avoids the issue but limits async concurrency.

## Recommended Solution

The ideal solution would be one of:

1. **Thread-safe GcCell**: Make GcCell operations thread-safe with proper synchronization when called from worker threads
2. **Cross-thread Gc tracking**: Allow AsyncHandleScope to properly track Gc across thread boundaries  
3. **Clear error message**: If cross-thread Gc access is unsupported, provide a clear compile-time or runtime error
4. **Documentation**: Document that Gc objects must not be written from tokio worker threads

## Additional Notes

- Single async task on main thread works fine (using spawn_local or blocking)
- The issue is specifically with multi-threaded tokio runtime
- This affects any reactive framework using rudo-gc with async resources
- The crash is non-deterministic but happens reliably when async tasks complete and try to update Gc-managed state
