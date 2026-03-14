# [Bug]: GcHandle::downgrade Missing is_allocated Check After inc_weak

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 downgrade 並發執行，slot 被回收並重用 |
| **Severity (嚴重程度)** | High | 錯誤地增加已被回收物件的 weak count，可能導致記憶體泄漏 |
| **Reproducibility (復現難度)** | High | 需要精確控制執行緒調度與 GC timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade` in `crates/rudo-gc/src/handles/cross_thread.rs:328` and `cross_thread.rs:344`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcHandle::downgrade` 函數沒有在調用 `inc_weak()` 之後檢查 `is_allocated`。這與正確的模式（`GcHandle::try_resolve()` 和 `Gc::downgrade()`）不同，導致 Time-Of-Check-Time-Of-Use (TOCTOU) race condition。

### 預期行為 (Expected Behavior)

應該先調用 `inc_weak()`，然後檢查 `is_allocated`。如果 slot 已被 sweep 且重用，應該 undo inc_weak 並返回 WeakCrossThreadHandle。

### 實際行為 (Actual Behavior)

當前程式碼 (cross_thread.rs:328):
```rust
gc_box.inc_weak();
// 缺少 is_allocated 檢查!
```

正確模式（來自 `GcHandle::try_resolve()` at cross_thread.rs:273-281）:
```rust
gc_box.inc_ref();  // 或 inc_weak

if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    let header = crate::heap::ptr_to_object_index(...);
    if !(*header.as_ptr()).is_allocated(idx) {
        crate::ptr::GcBox::dec_ref(...);  // undo
        return None;
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`GcHandle::downgrade()` 在調用 `gc_box.inc_weak()` 後缺少 `is_allocated` 檢查。當 lazy sweep 與 `downgrade()` 並發執行時：

1. `downgrade()` 開始執行
2. Lazy sweep 回收並重用 slot（分配新物件）
3. `downgrade()` 調用 `inc_weak()` - 錯誤地增加了新物件的 weak count
4. 返回 WeakCrossThreadHandle 指向新物件

這導致：
- 新物件的 weak count 被錯誤增加
- 當最後一個 strong reference 消失時，weak reference 仍然存在
- 記憶體泄漏或混淆

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發執行：
1. 一個執行緒調用 `GcHandle::downgrade()`
2. 另一個執行緒同時進行 lazy sweep

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_weak()` 之後添加 `is_allocated` 檢查，如果 slot 已被回收，undo inc_weak 並返回預設值：

```rust
gc_box.inc_weak();

// 添加 is_allocated 檢查
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        gc_box.dec_weak();  // undo
        // 處理方式：返回預設的 WeakCrossThreadHandle 或 panic
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Lazy sweep 與 mutate 並發時需要這種檢查模式
- 類似的 TOCTOU 問題在 bug240 (GcBox::as_weak) 中已修復

**Rustacean (Soundness 觀點):**
- 這是經典的 TOCTOU race condition
- 可能導致記憶體泄漏而非 UB，但仍然是不正確的行為

**Geohot (Exploit 觀點):**
- 需要精確的執行緒調度才能觸發
- 實際利用難度較高，但理論上可能導致記憶體消耗異常

---

## Resolution (2026-03-14)

**Outcome:** Fixed.

Reordered `GcHandle::downgrade` in `handles/cross_thread.rs` to call `inc_weak()` before the `is_allocated` check, matching the pattern used in `GcBox::as_weak` (bug240) and `Gc::downgrade`. When `is_allocated` fails after `inc_weak`, return a null `WeakCrossThreadHandle` without calling `dec_weak` (per bug133 — slot may be reused). Added `GcBoxWeakRef::null()` constructor. Both origin-alive and orphan paths updated. All tests pass.
