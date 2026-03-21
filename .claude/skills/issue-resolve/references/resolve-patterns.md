# Issue Resolution Patterns for rudo-gc

## Table of Contents
1. [SATB Barrier Missing / Overflow](#satb-barrier)
2. [TOCTOU / Race Conditions](#toctou-race)
3. [Dead/Dropping Flag Not Checked](#dead-dropping-flag)
4. [GcCapture Missing](#gccapture-missing)
5. [Atomic Ordering Issues](#atomic-ordering)
6. [Doc/Implementation Mismatch](#doc-mismatch)
7. [Invalid / Misidentified Issues](#invalid-issues)
8. [Status Update Rules](#status-update-rules)

---

## SATB Barrier Missing / Overflow {#satb-barrier}

**Symptoms**: GC misses writes; SATB invariant broken → UAF / premature collection.

**Key code locations**: `crates/rudo-gc/src/barrier.rs`, `gc_cell.rs`, `gc_thread_safe_cell.rs`

**Fix pattern**:
```rust
// Before mutation, record old value to SATB buffer
if let Err(_) = satb.record_satb_old_value(old_ptr) {
    // Buffer full → request a GC
    request_gc();
}
```

**Verification test strategy**:
1. `collect_full()` to promote objects to old gen
2. Create an OLD→YOUNG reference
3. Call `collect()` (minor only)
4. Assert YOUNG object is still alive

> ⚠️ Using `collect_full()` alone masks barrier bugs — always use minor GC to verify.

---

## TOCTOU / Race Conditions {#toctou-race}

**Symptoms**: `Weak::upgrade`, `Weak::clone`, `GcHandle::clone/unregister` — concurrent access between check and use.

**Fix pattern**: Replace non-atomic compare-swap with a single atomic operation:
```rust
// BAD: load then compare separately
let count = ref_count.load(Relaxed);
if count > 0 { ref_count.fetch_add(1, Acquire); }

// GOOD: CAS loop
ref_count.fetch_update(Acquire, Relaxed, |c| if c > 0 { Some(c + 1) } else { None })
```

**Ordering upgrade**: If `Relaxed` load is followed by a action that must observe the latest state, upgrade to `Acquire` (load) / `Release` (store).

**Verification**: Single-threaded PoC won't reliably trigger. Note in issue: "Requires Miri / ThreadSanitizer". Do NOT mark Invalid based on single-thread test passing.

---

## Dead/Dropping Flag Not Checked {#dead-dropping-flag}

**Symptoms**: `Gc::clone`, `Gc::deref`, `GcHandle::clone`, `downgrade` skip dead-object guard.

**Key flags**: `DEAD_FLAG`, `dropping_state` (in `GcBox` / `GcHeader`)

**Fix pattern**:
```rust
pub fn clone(&self) -> Self {
    let inner = self.inner();
    // Guard against dead or dropping objects
    assert!(!inner.has_dead_flag(), "clone on dead Gc");
    assert!(!inner.is_dropping(), "clone on dropping Gc");
    inner.inc_ref();
    Self { ptr: self.ptr }
}
```

**Where to look**: `crates/rudo-gc/src/gc.rs`, `gc_handle.rs`, `weak.rs`

---

## GcCapture Missing {#gccapture-missing}

**Symptoms**: Wrapper types (`GcMutex`, `Rc`, `Arc`, `RwLock`) don't implement `GcCapture` → GC roots inside them are invisible to the SATB barrier.

**Fix pattern**: Implement `GcCapture` for the wrapper, delegating to inner value:
```rust
unsafe impl<T: GcCapture> GcCapture for GcMutex<T> {
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<GcPtr>) {
        if let Ok(guard) = self.lock() {
            guard.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

> `try_read()` / `try_lock()` can miss GC ptrs if lock is contended. Use blocking variants when possible, or document the limitation.

---

## Atomic Ordering Issues {#atomic-ordering}

**Common mistake**: Using `Relaxed` for flag loads that gate safety-critical operations.

| Operation | Minimum correct ordering |
|---|---|
| Load flag then act (guard) | `Acquire` load |
| Store flag to signal other threads | `Release` store |
| Read-Modify-Write (CAS, fetch_add) | `AcqRel` or `SeqCst` |
| Pure statistics / metrics | `Relaxed` OK |

**GC_REQUESTED flag**: Must use at least `AcqRel` — a `Relaxed` load means a thread may miss the GC handshake signal.

---

## Doc/Implementation Mismatch {#doc-mismatch}

**Symptoms**: Docs say function panics on dead object, but code has no assert.

**Fix options**:
1. Add assertion to match docs (preferred if behavior change is safe)
2. Update docs to match actual behavior (if panic would be a regression)

**Template**:
```rust
/// # Panics
/// Panics if the object has been freed (dead flag set).
pub fn some_fn(&self) {
    assert!(!self.inner().has_dead_flag(), "called on dead object");
    // ...
}
```

---

## Invalid / Misidentified Issues {#invalid-issues}

Before marking `Status: Invalid`:
1. Verify with the "full GC masks barrier bugs" pattern (use minor GC)
2. Verify with "single-thread can't trigger races" pattern
3. Check if the container's `Gc` pointers are registered as roots

If genuinely invalid: set `Status: Invalid`, add a brief explanation, **do not fix the code**.

---

## Status Update Rules {#status-update-rules}

After resolving, always update **both** the issue file and `ISSUES_REPORT.md`.

| Outcome | Status | Tags |
|---|---|---|
| Bug confirmed, fix applied and tested | `Fixed` | `Verified` |
| Bug confirmed, fix applied, test inconclusive | `Fixed` | `Not Verified` |
| Bug is real but no fix yet, PoC works | `Open` | `Verified` |
| Bug is real but PoC didn't reproduce | `Open` | `Not Reproduced` |
| Bug is a misidentification | `Invalid` | `Not Verified` (or `Not Reproduced`) |

### Updating ISSUES_REPORT.md

After changing any issue status:
1. Update the statistics block (Fixed / Open counts, Tags counts)
2. Update the table row for the changed issue
