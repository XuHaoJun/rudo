# GC Corruption Bug Report

## Summary

A memory corruption bug was found in `rudo-gc` where `Vec<Gc<T>>` contents are corrupted during GC operations. The bug manifests as data from later rounds overwriting data from earlier rounds.

## Reproduction

**Test case**: `tests/vec_gc_corruption_minimal.rs`

Run with:
```bash
cargo test --test vec_gc_corruption_minimal -- --test-threads=1
```

## Corruption Pattern

During round 10 of component creation:
- **After push 10** (creating component id=10010):
  - Index 113: expected id=5013 (round 5, local 13), got id=10008 (round 10, local 8)
  - Index 114: expected id=5014, got id=10009
  - Index 115: expected id=5015, got id=10010

- **After push 15** (creating component id=10015):
  - Indices 113-119 corrupted (ids 10008-10014)
  - Index 120: expected id=6000 (round 6), got id=10015

## Key Findings

1. **No GC = No Corruption**: Tests pass when `set_collect_condition(|_| false)` disables automatic GC

2. **Explicit GC during loop causes corruption**:
   - GC at end of loop passes
   - GC during loop (every 5 rounds) causes corruption

3. **Corruption happens DURING component creation**, not during GC:
   - Corruption detected during round 10 before collect() is called
   - Specifically after push operations in round 10

4. **Corruption is in Vec contents**:
   - Gc pointers at correct indices (verified by accessing `comp.id`)
   - Component `id` field is corrupted with values from later rounds
   - This suggests memory content corruption, not pointer corruption

## Suspected Root Cause

The corruption pattern (indices 113-120 containing data from round 10) suggests:

1. **Vec push buffer overflow**: Something during `Vec::push` is writing past the Vec's capacity
2. **Write barrier writing to wrong address**: The generational write barrier may be calculating incorrect indices
3. **Page boundary issues**: BiBOP page calculation may be incorrect for the GcCell location

## Investigation Points

1. Check `GcCell::borrow_mut()` in `cell.rs` - does the generational write barrier calculate correct indices?
2. Verify `ptr_to_page_header()` and `ptr_to_object_index()` handle nested GcCell<Vec<Gc<T>>> correctly
3. Check if Vec reallocation triggers any problematic write barrier behavior

## Test Isolation

- `test_vec_no_auto_gc`: PASSES (no GC during test)
- `test_vec_gc_corruption_with_barriers`: FAILS (GC during test)
- Corruption location: Index 113 (should contain id=5013, contains id=10008)
