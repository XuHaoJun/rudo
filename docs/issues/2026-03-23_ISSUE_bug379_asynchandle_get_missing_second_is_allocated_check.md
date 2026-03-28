# [Bug]: AsyncHandle::get() Missing is_allocated Check Between dec_ref and value()

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during Handle::get() |
| **Severity (嚴重程度)** | `Critical` | Potential UAF - reading value from swept slot |
| **Reproducibility (復現難度)** | `Medium` | Requires precise thread timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()` and `AsyncHandle::get_unchecked()` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`AsyncHandle::get()` 應該在讀取值之前驗證 slot 仍然處於 allocated 狀態。與 `Handle::get()` (bug372) 的模式一致。

### 實際行為 (Actual Behavior)

`AsyncHandle::get()` 在 `dec_ref()` 和 `value()`之間缺少 `is_allocated` 檢查，導致潛在的 UAF。

**`Handle::get()` (已修復 - bug372):**
```rust
// Second is_allocated check after dec_ref (bug372 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!((*header.as_ptr()).is_allocated(idx), ...);
}

// Recheck flags before reading value
if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 || gc_box.is_under_construction() {
    panic!("...");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
let value = gc_box.value();  // Safe
```

**`AsyncHandle::get()` (有 bug):**
```rust
// is_allocated check happens BEFORE dec_ref (line 644-650)
if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    let header = crate::heap::ptr_to_page_header(...);
    assert!((*header.as_ptr()).is_allocated(idx), ...);  // Line 644-650
}

// Flags check and undo_inc_ref if bad
if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 || gc_box.is_under_construction() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("...");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());  // Line 663
let value = gc_box.value();  // BUG: No is_allocated check after dec_ref!
value
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

Race Condition 詳細過程：

1. Object A 存在於 slot，generation = 1, refcount = 1
2. Thread A 呼叫 `AsyncHandle::get()`
3. Thread A 通過 `is_allocated` 檢查（line 644-650） - 通過
4. Thread A 通過 flags 檢查（line 652-661）
5. Thread A 呼叫 `try_inc_ref_if_nonzero()` - refcount 變為 2（line 635）
6. Thread A 執行 generation assertion - 通過（line 638-642）
7. **Race Window**: Lazy sweep 在此時運行:
   - 確認 Object A 已死亡，回收 slot
   - 設置 `is_allocated = false`
   - 調用 `drop_fn` 丟棄 Object A 的值
8. Thread A 執行 `dec_ref()` - refcount 變為 1（line 663）
9. Thread A 執行 `value()` - **讀取已丟棄的值！**（line 664）- **UAF!**

Generation 檢查無法捕獲此問題，因為：
- Generation 只在 slot **重用** 時改變
- 如果 slot 被 sweep 後**未立即重用**，generation 保持不變

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確控制執行緒調度的並發測試環境
// 1. 建立 AsyncHandleScope 和 AsyncHandle
// 2. 減少 refcount 到 1（使物件符合 sweep 條件）
// 3. 在另一執行緒觸發 lazy sweep 回收 slot
// 4. 同時呼叫 AsyncHandle::get()
// 5. 觀察: 是否讀取到已丟棄的值
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 的 `dec_ref()` 和 `value()`之間添加 `is_allocated` 檢查：

```rust
// After dec_ref(), before value():
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());

// Second is_allocated check after dec_ref (bug372 pattern)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "AsyncHandle::get: object slot was swept after dec_ref"
    );
}

let value = gc_box.value();
value
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 和 handle access 的並發是一個已知的 race window。`Handle::get()` 在 bug372 中修復了這個問題。`AsyncHandle::get()` 應該採用相同模式。

**Rustacean (Soundness 觀點):**
如果 slot 在 `dec_ref()` 和 `value()`之間被 sweep，則 `value()` 會讀取已丟棄的記憶體。這是 UAF - 使用已釋放的記憶體。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制 GC timing 和物件生命週期，可以利用這個 race condition 來讀取其他物件的殘留資料。

---

## 驗證記錄

**驗證日期:** 2026-03-23

**驗證方法:**
- Code review 比較 `AsyncHandle::get()` (async.rs:600-667) 與 `Handle::get()` (mod.rs:302-358)
- 確認: `Handle::get()` 在 dec_ref 和 value()之間有 is_allocated 檢查（bug372 fix）
- 確認: `AsyncHandle::get()` 在 dec_ref (line 663) 和 value() (line 664) 之間**沒有** is_allocated 檢查
- 確認: `AsyncHandle::get_unchecked()` 也有同樣問題（line 756-757）
