# [Bug]: GcVisitorConcurrent::route_reference missing generation check after set_mark

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep and parallel marking |
| **Severity (嚴重程度)** | `High` | Could incorrectly clear mark on newly allocated object during concurrent marking |
| **Reproducibility (重現難度)** | `Medium` | Requires precise concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `Trace`, `GcVisitorConcurrent::route_reference` in `trace.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

After `set_mark` succeeds but `is_allocated` returns false, the code should verify the **generation** hasn't changed to distinguish between:
1. Slot was swept (slot still contains same object, mark should be cleared)
2. Slot was swept AND reused (slot contains new object with different generation, mark should NOT be cleared as it now belongs to the new object)

### 實際行為 (Actual Behavior)

The code clears the mark unconditionally when `is_allocated` fails after a successful `set_mark`, without checking the generation. This can incorrectly clear the mark on a **newly allocated object** that just happens to be in a swept slot.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `crates/rudo-gc/src/trace.rs`, lines 188-194:

```rust
if !(*header.as_ptr()).set_mark(idx) {
    return;
}
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

The issue: When `is_allocated` returns false after successful `set_mark`, the code clears the mark unconditionally. But if the slot was swept AND reused (a new object was allocated in the same slot with a different generation), clearing the mark would incorrectly clear the mark for the NEW object.

**Inconsistency with similar functions (bug336, bug355, bug360 pattern):**
- `scan_page_for_marked_refs` (incremental.rs): Has generation check after successful mark + is_allocated=false
- `scan_page_for_unmarked_refs` (incremental.rs): Has generation check after successful mark + is_allocated=false
- `mark_object_black` (incremental.rs): Has generation check after successful mark + is_allocated=false (bug355 fix)
- `mark_and_push_to_worker_queue` (gc.rs): Has generation check after successful mark + is_allocated=false (bug360 fix)
- `GcVisitorConcurrent::route_reference` (trace.rs): **MISSING generation check**

The bug was introduced in f71a01b which added the `set_mark` return value check but missed the generation check that was later established as the pattern in bug336/bug355/bug360 fixes.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Theoretical bug - requires specific concurrent interleaving
// 1. Thread A: Object A allocated in slot with generation 1
// 2. Thread A: Object A becomes unreachable
// 3. Thread B: Lazy sweep reclaims slot (generation remains 1)
// 4. Thread B: Object B allocated in same slot, generation increments to 2
// 5. Thread A: GcVisitorConcurrent::route_reference called on old Object A pointer
// 6. Thread A: set_mark succeeds (marks slot with generation 2's mark bit)
// 7. Thread A: is_allocated returns false (slot shows as unallocated)
// 8. Thread A: clear_mark_atomic is called - INCORRECTLY clearing Object B's mark!
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation check between `set_mark` and `clear_mark_atomic`, matching the pattern from bug355/bug360 fixes:

```rust
if !(*header.as_ptr()).set_mark(idx) {
    return;
}
// Read generation after successful mark to detect slot reuse
let marked_generation = unsafe { (*gc_box_ptr).generation() };
if !(*header.as_ptr()).is_allocated(idx) {
    // Verify generation hasn't changed to distinguish swept from swept+reused
    let current_generation = unsafe { (*gc_box_ptr).generation() };
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object, don't clear
        return;
    }
    // Slot was swept but not reused - safe to clear mark
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

Note: We need to get `gc_box_ptr` before the `set_mark` check since we need access to the GcBox to read generation.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**

The generation check is critical in concurrent GC systems where lazy sweep can reclaim and reuse slots concurrently with marking. The mark bit belongs to the slot, not the object - when a slot is reused, the new object inherits the mark bit state. Without a generation check, we cannot distinguish "slot was swept (same object)" from "slot was swept and reused (new object)".

**Rustacean (Soundness 觀點):**

This is a data race-adjacent issue. While not a direct UB like the PageHeader.generation data race (bug359), incorrect mark state can lead to use-after-free when objects are prematurely collected, or memory leaks when unreachable objects are kept alive.

**Geohot (Exploit 攻擊觀點):**

Incorrect GC mark state could potentially be exploited if an attacker can control allocation patterns to trigger specific slot reuse timing. However, the concurrent timing requirements make this difficult to weaponize reliably.

---

## 驗證記錄

**驗證日期:** 2026-03-21
**驗證人員:** opencode

### 驗證結果

確認 `GcVisitorConcurrent::route_reference` (trace.rs:188-194) 缺少 generation check：

- `set_mark` 成功後 `is_allocated` 返回 false 時，調用 `clear_mark_atomic` 沒有先檢查 generation
- 對比其他類似函數都有 generation check (bug336, bug355, bug360)
- 這是 f71a01b 引入了 `set_mark` 返回值檢查但漏掉了 generation check

**Status: Fixed** - 已在 trace.rs 中添加 generation check。

## Resolution (2026-03-21)

**Outcome:** Fixed in `crates/rudo-gc/src/trace.rs` (`GcVisitorConcurrent::route_reference`).

修復內容：
1. 在 `set_mark` 成功後讀取 `marked_generation`
2. 當 `is_allocated` 返回 false 時，檢查 `current_generation != marked_generation`
3. 如果 generation 改變（slot 被重用），不清除 mark，直接返回
4. 如果 generation 不變（slot 只被 sweep），安全地清除 mark

這與 bug336、bug355、bug360 中建立的模式一致。