//! Benchmark tests for GC marking optimizations.
//!
//! These benchmarks measure the performance improvements from:
//! - Push-based work transfer
//! - Segment ownership for load distribution
//! - Mark bitmap for memory efficiency

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use rudo_gc::gc::mark::MarkBitmap;

/// Benchmark mark bitmap memory overhead.
#[test]
fn benchmark_mark_bitmap_memory() {
    // Measure memory overhead of mark bitmap
    let bitmap = MarkBitmap::new(512);
    let bitmap_memory = std::mem::size_of::<MarkBitmap>() + bitmap.capacity() / 8;

    // Compare to forwarding pointers: 8 bytes per object
    let objects = 512;
    let forwarding_memory = objects * 8;

    // Bitmap should use ~64 bytes for 512 objects
    // Forwarding pointers would use 4096 bytes
    let overhead_reduction =
        ((forwarding_memory - bitmap_memory) as f64 / forwarding_memory as f64) * 100.0;

    println!(
        "Mark bitmap memory for 512 objects: {} bytes",
        bitmap_memory
    );
    println!(
        "Forwarding pointer overhead for 512 objects: {} bytes",
        forwarding_memory
    );
    println!("Memory reduction: {:.1}%", overhead_reduction);

    // Verify significant reduction (at least 95%)
    assert!(
        overhead_reduction >= 95.0,
        "Expected at least 95% reduction, got {:.1}%",
        overhead_reduction
    );
}

/// Test mark bitmap performance under concurrent access.
#[test]
fn benchmark_bitmap_concurrent_access() {
    let bitmap = Arc::new(MarkBitmap::new(512));
    let num_threads = 4;
    let iterations = 1000;

    let barrier = Arc::new(Barrier::new(num_threads));
    let marked_count = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let bitmap = Arc::clone(&bitmap);
            let barrier = Arc::clone(&barrier);
            let marked_count = Arc::clone(&marked_count);
            thread::spawn(move || {
                barrier.wait();
                for i in 0..iterations {
                    let index = (thread_id * iterations + i) % 512;
                    unsafe {
                        bitmap.mark(index);
                    }
                    if unsafe { bitmap.is_marked(index) } {
                        marked_count.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // All threads should have marked nodes
    let total = marked_count.load(Ordering::SeqCst);
    println!("Concurrent mark operations: {}", total);

    // Verify bitmap state
    for i in 0..512 {
        unsafe {
            assert!(bitmap.is_marked(i), "Slot {} should be marked", i);
        }
    }
}

/// Benchmark mark bitmap operations.
#[test]
fn benchmark_bitmap_operations() {
    let mut bitmap = MarkBitmap::new(4096);
    let start = std::time::Instant::now();

    // Mark all slots
    for i in 0..4096 {
        unsafe { bitmap.mark(i) };
    }

    let mark_duration = start.elapsed();
    println!("Marking 4096 slots took: {:?}", mark_duration);

    // Check all slots
    let mut all_marked = true;
    let check_start = std::time::Instant::now();
    for i in 0..4096 {
        if !unsafe { bitmap.is_marked(i) } {
            all_marked = false;
            break;
        }
    }
    let check_duration = check_start.elapsed();
    println!("Checking 4096 slots took: {:?}", check_duration);

    assert!(all_marked, "All slots should be marked");

    // Clear and benchmark
    let clear_start = std::time::Instant::now();
    bitmap.clear();
    let clear_duration = clear_start.elapsed();
    println!("Clearing 4096 slots took: {:?}", clear_duration);

    // Verify cleared
    for i in 0..4096 {
        assert!(
            !unsafe { bitmap.is_marked(i) },
            "Slot {} should be cleared",
            i
        );
    }
}
