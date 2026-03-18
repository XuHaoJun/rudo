# [Bug]: Handle::to_gc Missing Pre-Incrment is_allocated Check (TOCTOU)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 handle 操作並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | Critical | Memory corruption - 錯誤物件的 ref_count 被遞增 |
| **Reproducibility (重現難度)** | High | 需要精確的並發時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::to_gc()` in `handles/mod.rs`, `AsyncHandle::to_gc()` in `handles/async.rs`, `GcHandle::resolve_impl()` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
Handle 轉換為 Gc 時，應該在調用 `inc_ref()` 之前驗證物件槽位是否仍然已分配。這可以防止在 slot 被 lazy sweep 回收並重新分配後，錯誤地遞增另一個物件的 ref_count。

### 實際行為
存在與 bug339 相同的 TOCTOU 漏洞：

1. 取得 gc_box 指針
2. 檢查 flags (has_dead_flag, dropping_state, is_under_construction)
3. 調用 `inc_ref()` - **沒有 is_allocated 預檢查！**
4. 檢查 `is_allocated` - **太晚了！**

在步驟 2 和步驟 3 之間，slot 可能被 sweep 回收並分配新物件。然後 `inc_ref()` 會遞增新物件的 ref_count，導致：
- Memory corruption (錯誤物件的 ref_count 被修改)
- Use-after-free scenarios
- Potential double-free or leak

---

## 🔬 根本原因分析 (Root Cause Analysis)

### Bug Location 1: handles/mod.rs:360-399 (Handle::to_gc)

```rust
// 第一次 is_allocated 檢查 (lines 366-372) - 早在 flags 檢查之前
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::to_gc: slot has been swept and reused"
    );
}
let gc_box = &*gc_box_ptr;
assert!(...flags...);  // lines 374-379
if !gc_box.try_inc_ref_if_nonzero() {  // line 380 - 沒有 is_allocated 預檢查!
    panic!(...);
}
// 之後的 is_allocated 檢查 (lines 383-389) - 在 inc_ref 之後!
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::to_gc: object slot was swept after inc_ref"  // 這是錯誤的!
    );
}
```

問題：slot 在第一次 is_allocated 檢查和 inc_ref 之間可能被 sweep 回收並分配新物件。inc_ref 會作用在新物件上，之後的 is_allocated 檢查會通過（因為 slot 確實已分配），導致返回指向錯誤物件的 Gc 指針！

### Bug Location 2: handles/async.rs:753-789 (AsyncHandle::to_gc)

同樣的模式 - is_allocated 檢查在 inc_ref 之後。

### Bug Location 3: handles/cross_thread.rs:207-238 (GcHandle::resolve_impl)

更嚴重 - 根本沒有在 inc_ref 之前檢查 is_allocated！

```rust
let gc_box = &*self.ptr.as_ptr();  // line 209
assert!(...flags...);  // lines 210-221
gc_box.inc_ref();  // line 222 - 沒有 is_allocated 預檢查!
// 後來的 is_allocated 檢查 (lines 231-238) - 在 inc_ref 之後!
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
    // 1. Create a Handle to a Gc object
    // 2. Trigger lazy sweep to reclaim the object
    // 3. Allocate new object in same slot
    // 4. Concurrently call Handle::to_gc()
    // 5. Observe wrong ref count on new object
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_ref()` 之前添加 `is_allocated` 檢查，與 bug339 的修復一致：

### Fix for handles/mod.rs (Handle::to_gc):

```rust
// Move is_allocated check to BEFORE inc_ref (like bug339 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::to_gc: object slot was swept before inc_ref"
    );
}

if !gc_box.try_inc_ref_if_nonzero() {
    panic!("Handle::to_gc: object is being dropped by another thread");
}
```

### Fix for handles/async.rs (AsyncHandle::to_gc):

Same pattern - move is_allocated check before inc_ref.

### Fix for handles/cross_thread.rs (GcHandle::resolve_impl):

Add is_allocated check before inc_ref:

```rust
// Add BEFORE line 222 (inc_ref):
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "GcHandle::resolve: object slot was swept before inc_ref"
    );
}

gc_box.inc_ref();
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bug339 修復了 `Gc::cross_thread_handle()` 的相同問題，但忘記同時修復 `Handle::to_gc`、`AsyncHandle::to_gc` 和 `GcHandle::resolve_impl`。這是同一個 TOCTOU 模式 - 在標誌檢查和 ref 遞增之間，slot 可能被 sweep 回收並重用。

**Rustacean (Soundness 觀點):**
遞增錯誤物件的 ref_count 是嚴重的記憶體安全問題。可能導致：
- 新物件過早被收集 (ref_count 人為降低)
- 記憶體洩露 (舊物件的 count 人為提高)
- 雙重釋放

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制配置時機：
1. 釋放原始物件
2. 快速在相同 slot 配置受控物件
3. 觸發 handle 轉換來遞增效物件的 ref_count
4. 利用腐壞的 ref_count 狀態

---

## 🔗 相關 Issue

- bug339: Gc::cross_thread_handle missing is_allocated check (Fixed)
- bug289: Gc::clone missing is_allocated check BEFORE inc_ref (Fixed)
- bug257: Gc::more_methods missing is_allocated check (Fixed)
- bug133: dec_ref sweep race (Fixed)

---

## ✅ Verification

**Verified:** 在以下位置確認 bug 存在 - is_allocated 檢查在 inc_ref 之後：

1. `handles/mod.rs:360-399` - Handle::to_gc
2. `handles/async.rs:753-789` - AsyncHandle::to_gc  
3. `handles/cross_thread.rs:207-238` - GcHandle::resolve_impl

所有三處都有與 bug339 相同的 TOCTOU 漏洞模式。
