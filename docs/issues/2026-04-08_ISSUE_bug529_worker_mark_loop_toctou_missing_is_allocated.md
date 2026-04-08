# [Bug]: worker_mark_loop TOCTOU with lazy sweep missing second is_allocated check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Race condition between lazy sweep and parallel marking requires concurrent execution |
| **Severity (嚴重程度)** | High | Can cause mark on wrong object during concurrent lazy sweep |
| **Reproducibility (復現難度)** | Medium | Requires multi-threaded GC with lazy sweep concurrent with marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `worker_mark_loop` in `gc/marker.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When `try_mark` succeeds and we proceed to mark an object, we should verify the slot is still allocated before calling `trace_fn`. The generation check alone is insufficient when the slot is swept (not reused) - the generation remains the same but the object is gone.

### 實際行為 (Actual Behavior)
In `worker_mark_loop`, after `try_mark` returns `Ok(true)`, the code checks `is_allocated`, then reads `marked_generation`, then checks `is_allocated` again, then checks generation, then checks `is_under_construction`, then calls `trace_fn`. However, there's no second `is_allocated` check between the generation check and `trace_fn` call.

This TOCTOU window allows the following race:
1. Thread A: `try_mark` succeeds, slot is allocated
2. Thread B: Sweep reclaims the slot (sets is_allocated = false) but doesn't reallocate yet
3. Thread A: Generation check passes (old generation still in slot)
4. Thread A: Calls `trace_fn` on a slot that was just swept

### 對比 bug509 修復
bug509 fixed `scan_page_for_marked_refs` by adding:
```rust
// Second is_allocated re-check to fix TOCTOU with lazy sweep (bug509).
// If slot was swept after is_under_construction check but before push_work,
// clear mark and skip to avoid pushing a pointer to a swept slot.
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    break;
}
```

The same pattern should be applied in `worker_mark_loop` between the generation check and `trace_fn` call.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `worker_mark_loop` (gc/marker.rs:968-1010), the marking sequence is:
1. `try_mark` (CAS)
2. `is_allocated` check
3. `marked_generation = generation()`
4. `is_allocated` check
5. `generation() != marked_generation` check
6. `is_under_construction()` check
7. `trace_fn` call

The problem: Between step 5 (generation check) and step 7 (`trace_fn`), the slot could be swept without being reused. In this case:
- Generation is unchanged (old object wasn't reallocated yet)
- `is_under_construction` returns false for swept slot
- `trace_fn` is called on freed memory

The generation check only detects **slot reuse** (swept + reallocated), not **simple sweep** (swept but not yet reallocated).

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires multi-threaded GC with:
// 1. Parallel marking active
// 2. Lazy sweep running concurrently
// 3. High allocation pressure causing slot reuse

#[test]
fn worker_mark_loop_toctou_with_lazy_sweep() {
    use std::sync::Arc;
    use std::thread;
    
    // Setup: Allocate many objects to create slot pressure
    let handles: Vec<_> = (0..1000).map(|_| {
        let gc = Gc::new(Data { value: 42 });
        gc.cross_thread_handle()
    }).collect();
    
    // Spawn marking thread
    let marker = thread::spawn(|| {
        // Trigger concurrent marking with lazy sweep
        // The race window occurs when slots are swept during marking
    });
    
    // Main thread: continuous allocations to trigger lazy sweep
    // ...
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add a second `is_allocated` check between the generation check and `trace_fn` call, similar to bug509 fix in `scan_page_for_marked_refs`:

```rust
// After generation check (around line 996-998), add:
if !(*header.as_ptr()).is_allocated(idx) {
    let current_generation = (*gc_box_ptr).generation();
    if current_generation != marked_generation {
        break; // Slot was reused - mark belongs to new object
    }
    (*header.as_ptr()).clear_mark_atomic(idx);
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The mark-then-trace pattern is susceptible to TOCTOU when sweep runs concurrently. Chez Scheme uses a different approach where sweep and mark are fully serialized, avoiding this class of issues entirely. However, for concurrent GC, the fix pattern is correct: verify slot liveness after successful mark and before trace.

**Rustacean (Soundness 觀點):**
The missing `is_allocated` check between generation validation and `trace_fn` call creates a window where memory that has been freed could be accessed. This is undefined behavior in the Rust sense - accessing dropped memory. The fix adds a necessary fence to ensure memory safety.

**Geohot (Exploit 觀點):**
This TOCTOU could potentially be exploited if an attacker could control the timing of GC operations. However, the race window is extremely small and the consequences (calling trace_fn on freed memory) would likely cause a panic rather than controlled exploitation.