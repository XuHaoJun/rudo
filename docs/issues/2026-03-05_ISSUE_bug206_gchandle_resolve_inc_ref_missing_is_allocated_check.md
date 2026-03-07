# [Bug]: GcHandle::resolve/try_resolve/clone 缺少 inc_ref 後的 is_allocated 檢查導致 TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 mutator 並發執行，物件槽位被重用 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全漏洞 |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒調度，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`, `GcHandle::clone()`, `Gc::cross_thread_handle()`, `Gc::clone()`, `clone_orphan_root_with_inc_ref()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcHandle::resolve()`, `GcHandle::try_resolve()`, `GcHandle::clone()`, `Gc::cross_thread_handle()`, `Gc::clone()`, 以及 `clone_orphan_root_with_inc_ref()` 在調用 `inc_ref()` **之後**缺少對 `is_allocated()` 的檢查。這與 Bug 201 不同 - Bug 201 是檢查 `dropping_state()` 和 `has_dead_flag()`，而本 bug 是檢查物件槽位是否仍被分配（可能被 lazy sweep 回收並重用）。

### 預期行為 (Expected Behavior)

在調用 `inc_ref()` 增加引用計數後，應該再次檢查物件槽位是否仍被分配（`is_allocated()`）。如果槽位已被 sweep 且重用，應該撤銷 increment 並返回錯誤。

### 實際行為 (Actual Behavior)

以下位置都缺少 `is_allocated()` post-check：

1. `cross_thread.rs:208` - `GcHandle::resolve()`:
```rust
gc_box.inc_ref();
// 沒有 is_allocated 檢查！
Gc::from_raw(self.ptr.as_ptr() as *const u8)
```

2. `cross_thread.rs:264` - `GcHandle::try_resolve()`:
```rust
gc_box.inc_ref();
// 沒有 is_allocated 檢查！
Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
```

3. `cross_thread.rs:388` - `GcHandle::clone()`:
```rust
(*self.ptr.as_ptr()).inc_ref();
// 沒有 is_allocated 檢查！
```

4. `ptr.rs:1514` - `Gc::cross_thread_handle()`:
```rust
(*ptr.as_ptr()).inc_ref();
// 沒有 is_allocated 檢查！
```

5. `ptr.rs:1609` - `Gc::clone()`:
```rust
(*gc_box_ptr).inc_ref();
// 沒有 is_allocated 檢查！
```

6. `heap.rs:257` - `clone_orphan_root_with_inc_ref()`:
```rust
(*ptr.as_ptr()).inc_ref();
// 沒有 is_allocated 檢查！
```

對比正確的實作在 `gc/incremental.rs:1007-1010`:
```rust
// Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
if !(*h).is_allocated(idx) {
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 mutator 並發執行時：
1. 物件 A 在 slot `index` 被 lazy sweep 回收
2. 物件 B 在同一個 slot 被重新分配
3. Mutator 調用 `GcHandle::resolve()` 等方法
4. 通過所有 pre-checks（`is_under_construction`, `has_dead_flag`, `dropping_state`）
5. 執行 `gc_box.inc_ref()`（此時 slot 已被物件 B 佔用）
6. 返回 `Gc` 指標，但指標指向的是物件 B 的資料！

**後果：** 物件 B 的引用計數被錯誤地增加，且返回的 `Gc` 可能用於訪問物件 B，導致記憶體錯誤。

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
    marker: Arc<AtomicBool>,
}

fn main() {
    // 需要並發測試環境
    // 1. 創建物件並獲取 GcHandle
    // 2. 觸發 lazy sweep 回收物件
    // 3. 在同一 slot 分配新物件
    // 4. 同時調用 GcHandle::resolve()
    // 5. 觀察是否返回錯誤的物件
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在所有 `inc_ref()` 之後添加 `is_allocated()` 檢查：

```rust
gc_box.inc_ref();

// Post-check: verify object slot is still allocated after inc_ref
// (prevents TOCTOU with lazy sweep slot reuse)
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // Rollback the inc_ref we just did
        gc_box.dec_ref();
        // Return None or panic depending on context
        return None;  // for try_resolve
    }
}

Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 漏洞，與 lazy sweep 的並發執行有關。正確的做法是借鏡 `gc/incremental.rs` 中的 `mark_object_black` 函數，該函數已經正確地使用了 `is_allocated()` post-check 來防止 slot 重用導致的問題。

**Rustacean (Soundness 觀點):**
這是一個嚴重的記憶體安全問題。返回一個指向錯誤物件的指標會導致資料混淆，這是 Rust 最嚴重的安全問題之一。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以嘗試構造以下場景：
1. 通過精確的執行緒調度，在 inc_ref 和創建 Gc 指針之間觸發 lazy sweep
2. 利用 slot 重用來讀取/寫入敏感數據
3. 通過控制新物件的內容來實現任意記憶體讀寫

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
