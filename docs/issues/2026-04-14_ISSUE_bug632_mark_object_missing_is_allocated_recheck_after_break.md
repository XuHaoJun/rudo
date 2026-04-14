# Issue: mark_object missing is_allocated recheck after break (TOCTOU with lazy sweep)

## Status: Open
## Tags: Unverified

## Threat Model

| Severity | Likelihood | Reproducibility | Fix Complexity |
|----------|------------|-----------------|---------------|
| High | Low | Rare | Low |

## Affected Component

- **Component**: `mark_object` in `crates/rudo-gc/src/gc/gc.rs`
- **OS**: Linux
- **Rust**: 1.75+

## Description

### Expected Behavior
`mark_object` should re-verify `is_allocated` after successfully marking an object and before reading its `generation()` field, to prevent TOCTOU races with lazy sweep.

### Actual Behavior
After the `break` at line 2470, `mark_object` directly reads `(*ptr.as_ptr()).generation()` at line 2483 without rechecking `is_allocated`. If lazy sweep deallocates the slot between the `break` and the generation read, this causes undefined behavior (reading from deallocated memory).

## Root Cause Analysis

### Code Location
`crates/rudo-gc/src/gc/gc.rs` lines 2469-2484

### The Bug
```rust
visitor.objects_marked += 1;
break;  // Line 2470 - window opens here
}
// No is_allocated recheck!
let enqueue_generation = (*ptr.as_ptr()).generation();  // Line 2483 - UNSAFE!
visitor.worklist.push((ptr, enqueue_generation));       // Line 2484
```

### Correct Pattern (from `mark_and_trace_incremental`)
`mark_and_trace_incremental` at lines 2537-2548 has the fix:
```rust
visitor.objects_marked += 1;
// FIX bug566: Re-verify is_allocated after successful mark, before reading generation.
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
let enqueue_generation = (*ptr.as_ptr()).generation();
```

## Internal Discussion Record

### R. Kent Dybvig (GC/memory)
The TOCTOU window between `break` and `generation()` read is problematic. Lazy sweep can run concurrently and deallocate the slot. Even though `try_mark` succeeded, the slot could be reclaimed before we read `generation()`. This is a classic race condition in concurrent GC.

### Rustacean (UB/Soundness)
Reading `generation()` from a deallocated slot is undefined behavior. The compiler can assume the pointer is valid and reorder or optimize away subsequent checks. This could lead to silent corruption or security vulnerabilities.

### Geohot (Exploits)
An attacker who can influence GC timing could potentially exploit this window. If they can trigger lazy sweep at a precise moment, they might cause the GC to read stale generation data or push invalid entries to the worklist.

## Suggested Fix

Add an `is_allocated` recheck after the `break` and before reading `generation()`:

```rust
visitor.objects_marked += 1;
// FIX bug632: Re-verify is_allocated after successful mark, before reading generation.
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
break;
}
let enqueue_generation = (*ptr.as_ptr()).generation();
```
