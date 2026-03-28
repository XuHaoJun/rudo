# [Bug]: dec_ref used instead of undo_inc_ref for rollback after try_inc_ref_if_nonzero

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent drop/GC during handle access |
| **Severity (嚴重程度)** | `Critical` | Memory leak - object never reclaimed |
| **Reproducibility (重現難度)** | `Medium` | Race condition with precise timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `AsyncHandle::get()`, `AsyncHandle::get_unchecked()`, `AsyncHandle::to_gc()`, `Handle::to_gc()`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.x`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當 `try_inc_ref_if_nonzero()` 成功後，如果檢測到物件變為 dead/dropping/under_construction，應該使用 `undo_inc_ref()` 回滾遞增（此函數會無視 flags 強制遞減）。

### 實際行為 (Actual Behavior)

程式碼使用 `dec_ref()` 來回滾，但 `dec_ref()` 有 early-return 邏輯：當 `DEAD_FLAG` 設定或 `is_under_construction()` 為 true 時，**不會遞減 ref_count** 就直接返回。

這導致 `try_inc_ref_if_nonzero()` 造成的 ref_count 增量無法被撤銷，物件永遠無法達到 ref_count=0，導致記憶體洩漏。

### 程式碼位置

**async.rs:656** - `AsyncHandle::get()`:
```rust
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());  // BUG: dec_ref returns early without decrementing!
    panic!("AsyncHandle::get: object became dead/dropping after dec_ref");
}
```

**async.rs:745** - `AsyncHandle::get_unchecked()`:
```rust
if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());  // BUG: same issue
    panic!("AsyncHandle::get_unchecked: object became dead/dropping after dec_ref");
}
```

**async.rs:846** - `AsyncHandle::to_gc()`:
```rust
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());  // BUG: same issue
    panic!("AsyncHandle::to_gc: object became dead/dropping after ref increment");
}
```

**mod.rs:432** - `Handle::to_gc()`:
```rust
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());  // BUG: same issue
    panic!("Handle::to_gc: object became dead/dropping after ref increment");
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`dec_ref()` 的 early-return 邏輯 (ptr.rs:172-183):

```rust
pub fn dec_ref(self_ptr: *mut Self) -> bool {
    loop {
        let dead_flag = this.weak_count_raw() & GcBox::<()>::DEAD_FLAG;
        if dead_flag != 0 {
            // Already marked as dead - return false WITHOUT decrementing!
            return false;
        }
        if this.is_under_construction() {
            // Object under construction - return false WITHOUT decrementing!
            return false;
        }
        // ... normal decrement logic ...
    }
}
```

`undo_inc_ref()` 的正確行為 (ptr.rs:231-236):

```rust
pub(crate) unsafe fn undo_inc_ref(self_ptr: *mut Self) {
    // Uses fetch_sub which ALWAYS decrements regardless of flags
    (*self_ptr).ref_count.fetch_sub(1, Ordering::Release);
}
```

ptr.rs:219-224 的註解明確說明：

> "Use this instead of dec_ref when rolling back a successful try_inc_ref_from_zero or try_inc_ref_if_nonzero CAS: dec_ref returns early without decrementing when DEAD_FLAG is set, leaving ref_count incorrectly at 1."

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 1. Create AsyncHandleScope and AsyncHandle to Gc<T>
// 2. From another thread, drop all strong references to T (ref_count = 1)
// 3. Before GC runs, call handle.get() on the AsyncHandle
// 4. try_inc_ref_if_nonzero() succeeds (ref_count: 1 -> 2)
// 5. Before reading value, has_dead_flag() becomes true (GC marks it)
// 6. dec_ref() is called but returns early (dead_flag set)
// 7. ref_count stays at 2, object is never collected - MEMORY LEAK!
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Replace `dec_ref` with `undo_inc_ref` in all four locations:

```rust
// Change from:
GcBox::dec_ref(gc_box_ptr.cast_mut());
// To:
unsafe { GcBox::undo_inc_ref(gc_box_ptr.cast_mut()) }
```

**Files to fix:**
1. `async.rs:656`
2. `async.rs:745`
3. `async.rs:846`
4. `mod.rs:432`

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
When a successful `try_inc_ref_if_nonzero()` CAS must be rolled back due to concurrent state change, the rollback must actually decrement. Using `dec_ref()` which has guard conditions that prevent decrement is incorrect for this rollback scenario.

**Rustacean (Soundness 觀點):**
This is a memory leak bug. The ref_count semantics are violated: we increment (expecting to own a ref), but the rollback doesn't release it. The object remains alive forever despite having no live references.

**Geohot (Exploit 攻擊觀點):**
While not directly exploitable for code execution, memory leaks can be leveraged for denial-of-service. An attacker could repeatedly trigger this leak to exhaust memory.