# [Bug]: Gc::weak_cross_thread_handle inc_weak 後缺少 is_allocated 檢查導致 TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Gc::weak_cross_thread_handle 並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | Medium | 可能導致錯誤地增加 weak count 到已釋放物件，記憶體錯誤 |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒調度，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::weak_cross_thread_handle()`, `ptr.rs:1555-1574`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Gc::weak_cross_thread_handle()` 在調用 `inc_weak()` **之後**缺少對 `is_allocated()` 的檢查。這與 Bug 217 (Weak::clone inc_weak 缺少 is_allocated 檢查) 和 Bug 218 (GcHandle::downgrade inc_weak 缺少 is_allocated 檢查) 為同一模式。

### 預期行為

在調用 `inc_weak()` 增加 weak reference count 後，應該再次檢查物件槽位是否仍被分配（`is_allocated()`）。如果槽位已被 sweep 且重用，應該撤銷 increment 並 panic。

### 實際行為

`Gc::weak_cross_thread_handle()` 實現 (ptr.rs:1555-1574):
```rust
unsafe {
    let gc_box = &*self.as_non_null().as_ptr();
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        "Gc::weak_cross_thread_handle: ..."
    );
    gc_box.inc_weak();  // <-- 沒有 is_allocated 檢查！
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 mutator 並發執行時：
1. 物件 A 在 slot 被 lazy sweep 回收
2. 物件 B 在同一個 slot 被重新分配
3. Mutator 調用 `Gc::weak_cross_thread_handle()`
4. 通過所有 pre-checks（has_dead_flag, dropping_state, is_under_construction）
5. 執行 `inc_weak()`（此時 slot 已被物件 B 佔用）
6. 返回 WeakCrossThreadHandle，但其 weak count 錯誤地增加到了物件 B！

**後果：** 物件 B 的 weak count 被錯誤地增加，導致記憶體管理錯誤。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 需要並發測試環境
    // 1. 創建 Gc 並調用 weak_cross_thread_handle()
    // 2. 觸發 lazy sweep 回收物件
    // 3. 在同一 slot 分配新物件
    // 4. 同時調用 weak_cross_thread_handle()
    // 5. 觀察 weak count 是否錯誤增加
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_weak()` 之後添加 `is_allocated()` 檢查：

```rust
gc_box.inc_weak();

// Post-check: verify object slot is still allocated after inc_weak
// (prevents TOCTOU with lazy sweep slot reuse)
if let Some(idx) = crate::heap::ptr_to_object_index(self.as_non_null().as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.as_non_null().as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // Rollback the inc_weak we just did
        gc_box.dec_weak();
        panic!("Gc::weak_cross_thread_handle: object was swept during handle creation");
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 漏洞，與 lazy sweep 的並發執行有關。修復方案應參考 bug217 和 bug218 的修復方式，三者是同一模式。

**Rustacean (Soundness 觀點):**
這可能導致 weak count 管理錯誤，雖然不會直接導致 UAF，但會導致物件無法正確釋放。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以嘗試構造場景，通過精確控制 GC 時序，在 inc_weak 和返回之間觸發 lazy sweep，導致 weak count 計算錯誤。

---

## 修復狀態

- [x] 已修復
- [ ] 未修復

---

## Resolution (2026-03-14)

**Outcome:** Already fixed.

The fix was applied in the current codebase. `Gc::weak_cross_thread_handle()` in `ptr.rs` (lines 1721–1728) correctly performs an `is_allocated(idx)` check after `inc_weak()`:

```rust
gc_box.inc_weak();

if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::weak_cross_thread_handle: object slot was swept after inc_weak"
    );
}
```

Behavior matches the pattern used in bug 218 (GcHandle::downgrade). The implementation uses `assert!` (panic) rather than rollback via `dec_weak` when the slot is swept, per bug133 guidance (dec_weak on a reused slot would target the wrong object).
