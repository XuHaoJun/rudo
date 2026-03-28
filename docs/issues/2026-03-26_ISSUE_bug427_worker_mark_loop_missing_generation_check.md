# [Bug]: worker_mark_loop missing generation check before trace_fn - slot reuse TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep to reuse slot between mark and trace |
| **Severity (嚴重程度)** | Critical | Calling trace_fn on wrong object data causes memory corruption |
| **Reproducibility (復現難度)** | High | Needs concurrent incremental marking + lazy sweep + parallel workers |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/marker.rs`, `worker_mark_loop` and `worker_mark_loop_with_registry`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`worker_mark_loop` should verify the slot has not been reused (generation unchanged) before calling `trace_fn`. If generation changed, the object was swept and reallocated - calling trace on the new object's data is incorrect.

### 實際行為 (Actual Behavior)
`worker_mark_loop` checks `is_allocated` and `is_marked` but NOT generation before calling `trace_fn`. If the slot was swept and reused between when work was pushed to the queue and when `worker_mark_loop` processes it, `trace_fn` is called on the new object's data.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `gc/marker.rs`, `worker_mark_loop` (lines 918-978) and `worker_mark_loop_with_registry` (lines 1055-1136) call `trace_fn` at lines 963 and 1102 respectively without verifying generation hasn't changed:

```rust
// worker_mark_loop - line 963
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    marked += 1;
    let gc_box_ptr = obj.cast_mut();
    ((*gc_box_ptr).trace_fn)(ptr_addr, &mut visitor);  // BUG: No generation check!
    break;
}
```

The bug426 fix in `trace_and_mark_object` (gc/incremental.rs:808-828) added generation checks:
```rust
let marked_generation = (*gc_box.as_ptr()).generation();
// ...
if (*gc_box.as_ptr()).generation() != marked_generation {
    return;  // Slot was reused - skip
}
(((*gc_box.as_ptr()).trace_fn)(data_ptr, &mut visitor);
```

The same pattern should be applied in `worker_mark_loop`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable parallel incremental marking with multiple worker threads
2. Allocate object A in slot S, add to work queue (generation = G)
3. Lazy sweep reclaims slot S, reallocates with new object B (generation = G+1)
4. Worker thread pops work for A from queue
5. `is_allocated` returns true (B is in the slot)
6. `is_marked` returns false (B hasn't been marked)
7. `try_mark` succeeds on B
8. `trace_fn` is called on B's data instead of A's

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation capture after successful mark and verification before calling `trace_fn`:

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    // FIX: Capture generation to detect slot reuse (bug426 pattern)
    let marked_generation = (*gc_box_ptr).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        let current_generation = (*gc_box_ptr).generation();
        if current_generation != marked_generation {
            break;  // Slot was reused - skip
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        break;
    }
    marked += 1;
    let gc_box_ptr = obj.cast_mut();
    // FIX: Verify generation hasn't changed before calling trace_fn
    if (*gc_box_ptr).generation() != marked_generation {
        break;  // Slot was reused - skip
    }
    ((*gc_box_ptr).trace_fn)(ptr_addr, &mut visitor);
    break;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The parallel marking code uses work queues populated during page scanning. When `worker_mark_loop` processes an entry, it assumes the entry is still valid. However, lazy sweep can reclaim and reuse slots between when an object is added to the queue and when `worker_mark_loop` processes it. Without a generation check, `trace_fn` operates on potentially uninitialized or wrong object data. This is a classic TOCTOU bug in concurrent GC systems.

**Rustacean (Soundness 觀點):**
Calling `trace_fn` on the wrong object's data is undefined behavior - we're treating memory containing object B as if it contains object A. Even if both objects implement `Trace`, the trace visitor may read/write fields at offsets specific to the original object type. This could cause memory corruption, use-after-free, or type confusion.

**Geohot (Exploit 觀點):**
If an attacker can influence the timing of lazy sweep relative to incremental marking, they could potentially cause `trace_fn` to be called on attacker-controlled data, leading to memory corruption or information disclosure. The generation mechanism exists specifically to detect this slot reuse - not using it is a critical oversight.

---

## Resolution (2026-03-28)

**Outcome:** Fixed in tree.

`gc/marker.rs` implements the bug426-style pattern in both `worker_mark_loop` and `worker_mark_loop_with_registry`: after `try_mark(Ok(true))`, the code captures `marked_generation`, re-checks `is_allocated` with generation comparison before clearing the mark, verifies generation again immediately before `trace_fn`, and skips tracing when the slot was reused.

Verification: `bash test.sh` (workspace, `--all-features`, `--test-threads=1`) passed.
