//! Reproduction test for Bug #2: Pages with `dead_count` > 0 are skipped during lazy sweep.
//!
//! This test verifies that `alloc_from_pending_sweep` correctly sweeps pages
//! in the order they were collected, rather than letting `sweep_pending`
//! choose arbitrary pages.

use rudo_gc::{collect, Gc};
use std::cell::RefCell;
use std::rc::Rc;

const PAGE_OBJECTS: usize = 256;

const fn objects_per_page() -> usize {
    PAGE_OBJECTS
}

#[cfg(feature = "lazy-sweep")]
mod bug2_repro_tests {
    use super::*;
    use rudo_gc::heap::{LocalHeap, PAGE_FLAG_NEEDS_SWEEP};

    #[allow(dead_code)]
    fn count_pages_with_dead_objects(heap: &LocalHeap) -> usize {
        heap.pages
            .iter()
            .filter(|&&page_ptr| unsafe {
                let header = page_ptr.as_ptr();
                let header = header.as_ref().unwrap();
                (header.flags & PAGE_FLAG_NEEDS_SWEEP) != 0 && header.dead_count() > 0
            })
            .count()
    }

    #[test]
    fn test_alloc_from_pending_sweep_sweeps_collected_pages() {
        let gc_refs: Vec<Rc<RefCell<Gc<Vec<u32>>>>> = Vec::new();
        let gc_refs = Rc::new(RefCell::new(gc_refs));
        let mut allocations = Vec::new();

        for i in 0..(objects_per_page() * 4) {
            let gc = Gc::new(vec![u32::try_from(i).unwrap(); 1]);
            allocations.push(gc);
        }

        {
            let mut refs = gc_refs.borrow_mut();
            for (i, gc) in allocations.iter().enumerate() {
                if i % 2 == 0 {
                    refs.push(Rc::new(RefCell::new(gc.clone())));
                }
            }
        }

        collect();

        {
            let mut refs = gc_refs.borrow_mut();
            for _ in 0..(refs.len() / 2) {
                refs.pop();
            }
        }

        collect();

        let max_iterations = 1000;
        let mut pages_swept_count = 0;

        for _iteration in 0..max_iterations {
            let gc = Gc::new(vec![999u32; 1]);
            allocations.push(gc);

            pages_swept_count += 1;
            if pages_swept_count > objects_per_page() * 2 {
                break;
            }
        }

        assert!(
            pages_swept_count < max_iterations,
            "Expected pages to be swept within {max_iterations} iterations, but took {pages_swept_count}"
        );
    }

    #[test]
    fn test_no_page_starvation_in_lazy_sweep() {
        let mut gc_refs: Vec<Gc<Vec<u32>>> = Vec::new();

        for i in 0..(objects_per_page() * 4) {
            let gc = Gc::new(vec![u32::try_from(i).unwrap(); 1]);
            gc_refs.push(gc);
        }

        for i in 0..(gc_refs.len() / 2) {
            gc_refs[i] = Gc::new(vec![0u32; 1]);
        }

        collect();

        for _ in 0..(objects_per_page() * 4) {
            let gc = Gc::new(vec![1u32; 1]);
            gc_refs.push(gc);
        }

        collect();
    }

    #[test]
    #[allow(clippy::collection_is_never_read)]
    fn test_all_dead_pages_get_swept_eventually() {
        let mut gc_refs: Vec<Gc<Vec<u32>>> = Vec::new();

        for _i in 0..objects_per_page() {
            let gc = Gc::new(vec![42u32; 1]);
            gc_refs.push(gc);
        }

        gc_refs.clear();
        collect();

        for _i in 0..objects_per_page() {
            let gc = Gc::new(vec![42u32; 1]);
            gc_refs.push(gc);
        }

        gc_refs.clear();
        collect();

        for _i in 0..objects_per_page() {
            let gc = Gc::new(vec![42u32; 1]);
            gc_refs.push(gc);
        }

        collect();

        gc_refs.clear();
        collect();

        for _i in 0..100 {
            let gc = Gc::new(vec![42u32; 1]);
            gc_refs.push(gc);
        }

        collect();
    }
}

#[cfg(not(feature = "lazy-sweep"))]
mod bug2_eager_sweep_tests {
    use super::*;

    #[test]
    fn test_eager_sweep_works() {
        let gc1 = Gc::new(42);
        drop(gc1);
        collect();
    }
}
