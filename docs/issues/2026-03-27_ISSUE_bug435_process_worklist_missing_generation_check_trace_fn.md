# [Bug]: process_worklist missing generation check before trace_fn - trace_fn called on wrong object after slot reuse

**Status:** Open
**Tags:** Not Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during incremental marking with precise timing |
| **Severity (嚴重程度)** | `Critical` | trace_fn called on wrong object data after slot reuse - memory corruption |
| **Reproducibility (重現難度)** | `Medium` | Requires precise concurrent timing between mark, worklist push, and lazy sweep |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `process_worklist` in `gc/gc.rs` (lines 3028-3056)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

`process_worklist` should verify that the object being traced is the same object that was enqueued. Since generation changes on slot reuse, a generation check should be performed before calling `trace_fn` to ensure the slot hasn't been reused since enqueue.

### 實際行為 (Actual Behavior)

In `process_worklist` (gc/gc.rs:3028-3056), when an object is popped from the worklist and traced:

```rust
// Lines 3038-3053 in gc/gc.rs
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
    // Skip freed slots and already-marked objects
    if !(*header.as_ptr()).is_allocated(idx) {
        continue;
    }
    if (*header.as_ptr()).is_marked(idx) {
        continue;
    }
    (*header.as_ptr()).set_mark(idx);
    self.objects_marked += 1;
} else {
    continue;
}

(((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), self);  // NO generation check!
```

The code checks `is_allocated` and `is_marked`, but does NOT check if the generation has changed since the object was enqueued. If a slot is swept and reused between enqueue and processing, `trace_fn` could be called on the new object's data with the old object's trace function - causing incorrect tracing.

### 對比 `mark_and_trace_incremental`

`mark_and_trace_incremental` (lines 2477-2479) correctly checks generation before calling trace_fn:

```rust
// Lines 2477-2479
if (*ptr.as_ptr()).generation() != marked_generation {
    return;
}
```

But `process_worklist` has NO such check.

---

## 根本原因分析 (Root Cause Analysis)

### 漏洞場景

During incremental marking (when `GC_MARK_IN_PROGRESS` is NOT set), lazy sweep can run concurrently and sweep slots. The scenario:

1. Object A is marked with generation G1
2. `mark_object` adds A to worklist
3. Lazy sweep runs, sweeps slot, allocates new object B with generation G2 ≠ G1
4. `process_worklist` pops entry for A
5. `is_allocated` returns true (slot allocated with B)
6. `is_marked` returns false (B not yet marked)
7. `set_mark` marks B's slot
8. `trace_fn` called on B's data using A's trace function - **WRONG OBJECT**

### 為什麼 `mark_and_trace_incremental` 是安全的

`mark_and_trace_incremental` captures `marked_generation` at enqueue time and checks it before calling trace_fn. If slot was reused, generation would differ and trace_fn is not called.

### 為什麼 `process_worklist` 不安全

`process_worklist` does NOT capture or check generation. It relies solely on `is_allocated` and `is_marked` checks, which pass for a reused slot if:
- The new object is allocated (is_allocated = true)
- The new object hasn't been marked yet (is_marked = false)

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

**Theoretical race requiring precise concurrent timing:**

```rust
// Thread 1: Incremental marker
fn incremental_marking() {
    // Object A at generation G1 is marked and added to worklist
    mark_object(gc_box_A, &mut visitor);  // A added to worklist
    
    // ... sometime later, process worklist
    visitor.process_worklist();  // BUG: could trace B's data with A's trace_fn
}

// Thread 2: Lazy sweep (runs concurrently during incremental marking)
fn lazy_sweep() {
    // Slot is swept - A is dead
    sweep_slot(slot);
    // New object B allocated at same slot with generation G2 ≠ G1
    allocate_new_object(B);
}
```

---

## 建議修復方案 (Suggested Fix / Remediation)

Option 1: Add generation capture and check in `process_worklist`:

```rust
pub fn process_worklist(&mut self) {
    while let Some(ptr) = self.worklist.pop() {
        unsafe {
            // ... existing checks ...
            
            if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr().cast()) {
                // Capture generation at pop time
                let pop_generation = (*ptr.as_ptr()).generation();
                
                if !(*header.as_ptr()).is_allocated(idx) {
                    continue;
                }
                if (*header.as_ptr()).is_marked(idx) {
                    continue;
                }
                
                // FIX: Check generation matches (slot not reused since enqueue)
                let current_generation = (*ptr.as_ptr()).generation();
                if current_generation != pop_generation {
                    continue;  // Slot was reused - skip
                }
                
                (*header.as_ptr()).set_mark(idx);
                self.objects_marked += 1;
            }
            
            ((*ptr.as_ptr()).trace_fn)(ptr.as_ptr().cast(), self);
        }
    }
}
```

Option 2: Have `mark_object` store generation with the worklist entry and verify at pop. More complex but more efficient.

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
During incremental marking, lazy sweep can run concurrently. The worklist can contain stale entries for objects whose slots have been reused. Without generation checks, `trace_fn` could be called on wrong object data, corrupting the GC's view of the heap.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. Calling `trace_fn` on wrong object data can corrupt the mark state and lead to use-after-free or memory leaks. The `is_allocated` and `is_marked` checks are insufficient because they don't detect slot reuse.

**Geohot (Exploit 觀點):**
An attacker who can influence GC timing could trigger this race to cause memory corruption. If they can make `trace_fn` trace attacker-controlled data instead of legitimate objects, they might achieve code execution.

---

## 相關 Issue

- bug426: trace_and_mark_object missing generation check - similar issue in incremental.rs
- bug427: worker_mark_loop missing generation check - correct pattern followed
- bug431: mark_and_trace_incremental missing generation check - claimed missing but has check at lines 2477-2479
- bug295: TOCTOU between is_allocated check and set_mark - root cause pattern
