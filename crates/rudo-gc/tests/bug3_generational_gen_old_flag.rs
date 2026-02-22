//! Test for Bug 3: Generational barrier ignores per-object `GEN_OLD_FLAG`
//!
//! When page generation=0 but object has `GEN_OLD_FLAG`, `generational_write_barrier`
//! only checks page generation, so OLD->YOUNG ref may not be recorded.
//!
//! See: docs/issues/2026-02-19_ISSUE_bug3_generational_barrier_gen_old_flag.md

use rudo_gc::{collect_full, Gc, GcCell, Trace};

#[derive(Clone, Trace)]
struct YoungData {
    value: i32,
}

#[derive(Trace)]
struct OldData {
    young_ref: GcCell<Gc<YoungData>>,
}

#[test]
fn test_generational_barrier_gen_old_flag() {
    let young_cell = GcCell::new(Gc::new(YoungData { value: 100 }));
    let old = Gc::new(OldData {
        young_ref: young_cell,
    });

    for _ in 0..10 {
        collect_full();
    }

    // OLD -> YOUNG write: store young Gc in old object's GcCell
    let young_obj = Gc::new(YoungData { value: 999 });
    *old.young_ref.borrow_mut() = young_obj;

    collect_full();

    // If bug exists: young_obj may be wrongly collected -> wrong value or UAF
    assert_eq!(
        old.young_ref.borrow().value,
        999,
        "Young object should survive (barrier must record OLD->YOUNG)"
    );
}
