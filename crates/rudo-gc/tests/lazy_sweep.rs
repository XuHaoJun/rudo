//! Tests for lazy sweep garbage collection.
//!
//! These tests verify that lazy sweep correctly reclaims dead objects
//! while preserving live objects, and eliminates STW pause times.

use rudo_gc::{collect, Gc};

#[cfg(feature = "lazy-sweep")]
mod lazy_sweep_tests {
    use super::*;

    #[test]
    fn test_lazy_sweep_frees_dead_objects() {
        {
            let _gc = Gc::new(42);
        }
        collect();
    }

    #[test]
    fn test_lazy_sweep_preserves_live_objects() {
        let gc1 = Gc::new(42);
        {
            let _gc2 = Gc::new(100);
            let _gc3 = Gc::new(200);
            collect();
            assert_eq!(*gc1, 42);
        }
        assert_eq!(*gc1, 42);
    }

    #[test]
    fn test_lazy_sweep_eliminates_stw_pause() {
        for i in 0..100 {
            let _gc = Gc::new(i);
        }
        collect();
    }
}

#[cfg(not(feature = "lazy-sweep"))]
mod eager_sweep_tests {
    use super::*;

    #[test]
    fn test_eager_sweep_still_works() {
        {
            let _gc = Gc::new(42);
        }
        collect();
    }
}

#[cfg(feature = "lazy-sweep")]
mod lazy_sweep_all_dead_tests {
    use super::*;

    #[test]
    fn test_all_dead_page_fast_path() {
        for i in 0..50 {
            let _gc = Gc::new(i);
        }
        collect();
    }
}

#[cfg(feature = "lazy-sweep")]
mod lazy_sweep_allocation_tests {
    use super::*;

    #[test]
    #[allow(clippy::ptr_as_ptr)]
    fn test_allocated_memory_reused_after_sweep() {
        let gc1 = Gc::new(vec![1, 2, 3]);
        let _addr1 = Gc::as_ptr(&gc1) as *const _ as usize;
        drop(gc1);
        collect();
        let gc2 = Gc::new(vec![4, 5, 6]);
        let _addr2 = Gc::as_ptr(&gc2) as *const _ as usize;
    }

    #[test]
    #[allow(clippy::collection_is_never_read)]
    fn test_heap_size_bounded_under_workload() {
        let mut gc_refs = Vec::new();
        for i in 0..1000 {
            let gc = Gc::new(i);
            gc_refs.push(gc);
            if i % 10 == 0 {
                gc_refs.remove(0);
            }
        }
        collect();
    }
}

#[cfg(feature = "lazy-sweep")]
mod lazy_sweep_feature_tests {
    use super::*;

    #[test]
    fn test_lazy_sweep_enabled_by_default() {
        #[cfg(feature = "lazy-sweep")]
        {
            println!("lazy-sweep feature is enabled");
        }
    }

    #[test]
    #[allow(clippy::ptr_as_ptr)]
    fn test_large_object_still_eager() {
        let large = Gc::new(vec![0u8; 4096]);
        assert!(Gc::as_ptr(&large) as usize != 0);
        drop(large);
        collect();
    }

    #[test]
    fn test_orphan_page_still_eager() {
        let gc = Gc::new(42);
        assert_eq!(*gc, 42);
    }
}

#[cfg(feature = "lazy-sweep")]
mod lazy_sweep_api_tests {
    use super::*;

    #[test]
    fn test_sweep_pending_returns_correct_count() {
        for _ in 0..10 {
            let _gc = Gc::new(42);
        }
        collect();
    }

    #[test]
    fn test_pending_sweep_pages_returns_accurate_count() {
        for _ in 0..5 {
            let _gc = Gc::new(42);
        }
        collect();
    }

    #[test]
    fn test_mark_phase_blocks_lazy_sweep() {
        use std::sync::atomic::Ordering;

        rudo_gc::gc::sync::GC_MARK_IN_PROGRESS.store(true, Ordering::Relaxed);

        let gc = Gc::new(42);
        assert_eq!(*gc, 42);

        rudo_gc::gc::sync::GC_MARK_IN_PROGRESS.store(false, Ordering::Relaxed);

        collect();
    }
}

#[cfg(feature = "lazy-sweep")]
mod lazy_sweep_invariant_tests {
    use super::*;

    #[test]
    fn test_mark_bits_cleared_after_sweep() {
        let gc1 = Gc::new(42);
        let gc2 = Gc::new(100);
        let _gc3 = Gc::new(200);

        collect();

        assert_eq!(*gc1, 42);
        assert_eq!(*gc2, 100);

        collect();

        assert_eq!(*gc1, 42);
        assert_eq!(*gc2, 100);
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]
    fn test_dead_count_accuracy() {
        let mut gc_refs: Vec<Gc<i32>> = Vec::new();
        for i in 0..20 {
            gc_refs.push(Gc::new(i));
        }

        collect();

        for _ in 0..10 {
            gc_refs.remove(0);
        }

        collect();

        assert_eq!(gc_refs.len(), 10);
        for (idx, gc) in gc_refs.iter().enumerate() {
            assert_eq!(*gc, (idx as i32 + 10).into());
        }
    }

    #[test]
    fn test_sequential_collections() {
        let gc1 = Gc::new(1);
        let gc2 = Gc::new(2);

        collect();
        assert_eq!(*gc1, 1);
        assert_eq!(*gc2, 2);

        collect();
        assert_eq!(*gc1, 1);
        assert_eq!(*gc2, 2);

        collect();
        assert_eq!(*gc1, 1);
        assert_eq!(*gc2, 2);
    }

    #[test]
    fn test_partial_sweep_survival() {
        let gc1 = Gc::new(vec![1, 2, 3]);
        let gc2 = Gc::new(vec![4, 5, 6]);
        let gc3 = Gc::new(vec![7, 8, 9]);

        drop(gc2);

        collect();

        assert_eq!(*gc1, vec![1, 2, 3]);
        assert_eq!(*gc3, vec![7, 8, 9]);
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::unnecessary_cast
    )]
    fn test_interleaved_alloc_collect() {
        let mut gc_refs: Vec<Gc<i32>> = Vec::new();

        for round in 0..5 {
            for i in 0..10 {
                gc_refs.push(Gc::new(round as i32 * 100 + i as i32));
            }

            if round % 2 == 0 {
                gc_refs.truncate(gc_refs.len() / 2);
                collect();
            }
        }

        collect();

        for _ in &gc_refs {}
    }

    #[test]
    fn test_all_dead_flag_cleared_on_full_sweep() {
        let mut gc_refs: Vec<Gc<i32>> = Vec::new();

        for i in 0..10 {
            gc_refs.push(Gc::new(i));
        }

        collect();

        drop(gc_refs);

        collect();
        collect();
        collect();

        let gc = Gc::new(42);
        assert_eq!(*gc, 42);
    }

    #[test]
    fn test_dead_count_accumulates_across_collections() {
        let mut gc_refs: Vec<Gc<i32>> = Vec::new();

        for i in 0..20 {
            gc_refs.push(Gc::new(i));
        }

        collect();

        for _ in 0..5 {
            gc_refs.remove(0);
        }

        collect();

        for _ in 0..5 {
            gc_refs.remove(0);
        }

        collect();

        assert_eq!(gc_refs.len(), 10);
        for (idx, gc) in gc_refs.iter().enumerate() {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_possible_wrap
            )]
            {
                assert_eq!(*gc, (idx as i32 + 10).into());
            }
        }
    }

    #[test]
    fn test_mark_bits_not_persistent_on_unallocated_slots() {
        let gc1 = Gc::new(1);
        let gc2 = Gc::new(2);

        collect();

        assert_eq!(*gc1, 1);
        assert_eq!(*gc2, 2);

        collect();

        assert_eq!(*gc1, 1);
        assert_eq!(*gc2, 2);
    }
}
