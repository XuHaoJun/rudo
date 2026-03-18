# [Bug]: Gc::cross_thread_handle Missing Pre-Incrment is_allocated Check (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Gc::cross_thread_handle 並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | Critical | Memory corruption - 錯誤物件的 ref_count 被遞增 |
| **Reproducibility (重現難度)** | High | 需要精確的並發時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::cross_thread_handle()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`Gc::cross_thread_handle()` 應該在調用 `inc_ref()` 之前驗證物件槽位是否仍然已分配。這可以防止在 slot 被 lazy sweep 回收並重新分配後，錯誤地遞增另一個物件的 ref_count。

### 實際行為
`Gc::cross_thread_handle()` 存在與 bug289 (Gc::clone) 相同的 TOCTOU 漏洞：

1. Lines 1835-1840: 檢查 flags (`has_dead_flag`, `dropping_state`, `is_under_construction`)
2. Line 1841: 調用 `inc_ref()` - **沒有 is_allocated 預檢查！**
3. Lines 1843-1850: 檢查 `is_allocated` - **太晚了！**

在步驟 1 和步驟 2 之間，slot 可能被 sweep 回收並分配新物件。然後 `inc_ref()` 會遞增新物件的 ref_count，導致：
- Memory corruption (錯誤物件的 ref_count 被修改)
- Use-after-free scenarios
- Potential double-free or leak

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1834-1851` (`Gc::cross_thread_handle`):

```rust
// Step 1: Check flags (lines 1835-1840)
assert!(
    !(*ptr.as_ptr()).has_dead_flag()
        && (*ptr.as_ptr()).dropping_state() == 0
        && !(*ptr.as_ptr()).is_under_construction(),
    "Gc::cross_thread_handle: cannot create handle for dead, dropping, or under construction Gc"
);

// Step 2: inc_ref - NO is_allocated check before this!
(*ptr.as_ptr()).inc_ref();  // LINE 1841

// Step 3: is_allocated check AFTER inc_ref - TOO LATE! (lines 1843-1850)
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::cross_thread_handle: object slot was swept after inc_ref"
    );
}
```

此問題與 bug289 相同，但 bug289 只修復了 `Gc::clone()`，沒有修復 `Gc::cross_thread_handle()`。

對比已修復的 `Gc::clone()` (ptr.rs:1975-1995):
```rust
// Check is_allocated BEFORE inc_ref to avoid TOCTOU (bug289).
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::clone: object slot was swept before inc_ref"
    );
}

(*gc_box_ptr).inc_ref();
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // Requires concurrent testing environment
    // 1. Create a Gc object
    // 2. Trigger lazy sweep to reclaim the object
    // 3. Allocate new object in same slot
    // 4. Concurrently call Gc::cross_thread_handle()
    // 5. Observe wrong ref count on new object
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_ref()` 之前添加 `is_allocated` 檢查，與 `Gc::clone()` 的修復一致：

```rust
// Add BEFORE line 1841:
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::cross_thread_handle: object slot was swept before inc_ref"
    );
}

(*ptr.as_ptr()).inc_ref();
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bug289 修復了 `Gc::clone()` 的相同問題，但忘記同時修復 `Gc::cross_thread_handle()`。這是同一個 TOCTOU 模式 - 在標誌檢查和 ref 遞增之間，slot 可能被 sweep 回收並重用。

**Rustacean (Soundness 觀點):**
遞增錯誤物件的 ref_count 是嚴重的記憶體安全問題。可能導致：
- 新物件過早被收集 (ref_count 人為降低)
- 記憶體洩露 (舊物件的 count 人為提高)
- 雙重釋放

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制配置時機：
1. 釋放原始物件
2. 快速在相同 slot 配置受控物件
3. 觸發 cross_thread_handle 來遞增效物件的 ref_count
4. 利用腐壞的 ref_count 狀態

---

## 🔗 相關 Issue

- bug289: Gc::clone missing is_allocated check BEFORE inc_ref (Fixed)
- bug257: Gc::more_methods missing is_allocated check (Fixed)
- bug133: dec_ref sweep race (Fixed)

---

## ✅ Verification

**已驗證:** 在 `ptr.rs:1834-1851` 確認 bug 存在 - is_allocated 檢查在 inc_ref 之後。

**修復已套用:** 在 `ptr.rs` 的 `Gc::cross_thread_handle()` 中添加了 is_allocated 預檢查（位於 inc_ref 之前），與 Gc::clone() 的修復模式一致。

修復內容：
```rust
// Check is_allocated BEFORE inc_ref to avoid TOCTOU with lazy sweep (bug339).
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::cross_thread_handle: object slot was swept before inc_ref"
    );
}
```
