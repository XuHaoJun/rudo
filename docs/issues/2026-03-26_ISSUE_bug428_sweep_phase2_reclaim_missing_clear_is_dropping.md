# [Bug]: sweep_phase2_reclaim missing clear_is_dropping causes memory leak on slot reuse

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any slot reclaimed during sweep that is reused |
| **Severity (嚴重程度)** | High | Memory leak - dropped values never have their Drop run |
| **Reproducibility (重現難度)** | Medium | Requires specific object lifecycle with sweep reclaiming slot |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sweep_phase2_reclaim` in `gc.rs`, `dec_ref` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

When a slot is reclaimed during sweep in `sweep_phase2_reclaim`, the `is_dropping` field is NOT cleared, unlike other flags (`DEAD_FLAG`, `GEN_OLD_FLAG`, `UNDER_CONSTRUCTION_FLAG`).

### 預期行為 (Expected Behavior)
When a slot is reclaimed during sweep, all GC-related flags should be reset so the slot starts with a clean state when eventually reused.

### 實際行為 (Actual Behavior)
If a slot is reclaimed via `sweep_phase2_reclaim` and later reused, the `is_dropping` flag persists from the previous object. When `dec_ref` is called on the new object with `ref_count == 1`:

```rust
// ptr.rs:189
if count == 1 && this.dropping_state() == 0 {
    if this.try_mark_dropping() {
        unsafe { (this.drop_fn)(self_ptr.cast::<u8>()); }
    }
}
```

Since `dropping_state != 0`, the code falls through to lines 208-214 which just decrements `ref_count` to 0 WITHOUT calling `drop_fn`. The new object's value is NEVER dropped - a memory leak!

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `gc.rs` at `sweep_phase2_reclaim` (lines 2309-2311):

```rust
(*header).clear_allocated(i);
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();
// MISSING: (*gc_box_ptr).clear_is_dropping();
```

Compare with `pop_from_free_list` in `heap.rs` (lines 2325-2328) which correctly clears all flags:
```rust
(*gc_box_ptr).clear_dead();
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();
(*gc_box_ptr).clear_is_dropping();  // <-- This IS called here
```

The bug408 fix addressed `pop_from_free_list` but missed the same issue in `sweep_phase2_reclaim`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This would need to be run with specific conditions:
// 1. Create object A
// 2. Let object A's ref_count go to 1 and enter dropping phase
// 3. Force sweep to reclaim A's slot (mark it dead)
// 4. Reuse the slot (alloc from free list)
// 5. Create new object B in the reused slot
// 6. Drop B - its value will NOT be dropped because dropping_state inherited from A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `(*gc_box_ptr).clear_is_dropping();` in `sweep_phase2_reclaim` at line 2311, alongside the other flag clearing operations:

```rust
(*header).clear_allocated(i);
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();
(*gc_box_ptr).clear_is_dropping();  // <-- Add this line
reclaimed += 1;
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Slot reuse detection via generation counter is working correctly, but `is_dropping` is a state flag that must also be reset. In Chez Scheme's GC, all object metadata is cleared on reuse to prevent stale state from affecting new allocations.

**Rustacean (Soundness 觀點):**
This is not a safety violation (no UB), but a memory leak. The `drop_fn` not being called means `Drop` implementations never run, which can cause resource leaks rather than memory safety issues.

**Geohot (Exploit 觀點):**
While not directly exploitable for memory corruption, uncontrolled resource leaks could lead to denial-of-service in long-running GC applications.

---

## Resolution (2026-03-28)

**Verified in code:** `sweep_phase2_reclaim` in `crates/rudo-gc/src/gc/gc.rs` clears `is_dropping` when reclaiming a dead slot (`(*gc_box_ptr).clear_is_dropping()` immediately after `clear_under_construction()`), matching `pop_from_free_list` in `heap.rs`. No further source change required for this issue.