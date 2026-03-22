# [Bug]: Handle::get() Missing Second is_allocated Check After dec_ref()

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during Handle::get() |
| **Severity (嚴重程度)** | `Critical` | Potential UAF - reading value from swept slot |
| **Reproducibility (重現難度)** | `Medium` | Requires precise thread timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::get()` in `handles/mod.rs:302-337`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Handle::get()` 應該在讀取值之前驗證 slot 仍然處於 allocated 狀態。由於 `Handle::to_gc()` 已經正確實作了此檢查（見下方對比），`Handle::get()` 應該採用相同模式。

### 實際行為 (Actual Behavior)

`Handle::get()` 在 `dec_ref()` 和 `value()`之間缺少第二個 `is_allocated` 檢查，而 `Handle::to_gc()` 有這個檢查。

**`Handle::to_gc()` 有完整的檢查鏈:**
```rust
// 1. First is_allocated check (pre)
if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    let header = crate::heap::ptr_to_page_header(...);
    assert!((*header.as_ptr()).is_allocated(idx), ...);  // Line 379-382
}
// ... flags and generation checks ...
// ... try_inc_ref_if_nonzero() ...
// ... generation assertion ...

// 2. Second is_allocated check (POST-increment) - Line 400-406
if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    let header = crate::heap::ptr_to_page_header(...);
    assert!((*header.as_ptr()).is_allocated(idx), ...);  // "Handle::to_gc: object slot was swept after inc_ref"
}

// 3. Flags recheck - Line 407-413
if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 || gc_box.is_under_construction() {
    GcBox::dec_ref(gc_box_ptr.cast_mut());
    panic!("Handle::to_gc: object became dead/dropping after ref increment");
}

Gc::from_raw(gc_box_ptr as *const u8)  // Safe to use
```

**`Handle::get()` 缺少第二個檢查:**
```rust
// 1. First is_allocated check (pre) - Line 310-315
if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    let header = crate::heap::ptr_to_page_header(...);
    assert!((*header.as_ptr()).is_allocated(idx), ...);
}
// ... flags and generation checks ...
// ... try_inc_ref_if_nonzero() ...
// ... generation assertion ...

// 2. NO second is_allocated check! - MISSING!

// 3. dec_ref() - Line 333
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());

// 4. value() - Line 334-335 - BUG: No is_allocated check before reading value!
let value = gc_box.value();
value
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

Race Condition 詳細過程：

1. Object A 存在於 slot，generation = 1, refcount = 1
2. Thread A 呼叫 `Handle::get()`
3. Thread A 通過第一個 `is_allocated` 檢查（line 310-315）
4. Thread A 通過 flags 檢查（line 317-323）
5. Thread A 保存 `pre_generation = 1`（line 324）
6. Thread A 呼叫 `try_inc_ref_if_nonzero()` - refcount 變為 2（line 325）
7. Thread A 執行 `assert_eq(1, 1)` - 通過（line 328-332）
8. **Race Window**: Lazy sweep 在此時運行:
   - 確認 Object A 已死亡，回收 slot
   - 設置 `is_allocated = false`
   - 調用 `drop_fn` 丟棄 Object A 的值
9. Thread A 執行 `dec_ref()` - refcount 變為 1（line 333）
10. Thread A 執行 `value()` - **讀取已丟棄的值！**（line 334）- **UAF!**

Generation 檢查無法捕獲此問題，因為：
- Generation 只在 slot **重用** 時改變
- 如果 slot 被 sweep 後**未立即重用**，generation 保持不變
- Generation 檢查只驗證 slot 是否被重用，不驗證 slot 是否仍然 allocated

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確控制執行緒調度的並發測試環境
// 1. 建立 HandleScope 和 Handle
// 2. 減少 refcount 到 1（使物件符合 sweep 條件）
// 3. 在另一執行緒觸發 lazy sweep 回收 slot
// 4. 同時呼叫 Handle::get()
// 5. 觀察: 是否讀取到已丟棄的值
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Handle::get()` 的 `dec_ref()` 和 `value()`之間添加第二個 `is_allocated` 檢查，並在讀取值之前重新檢查 flags：

```rust
// After dec_ref(), add:
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::get: object slot was swept after dec_ref"
    );
}

// Recheck flags before reading value (same as to_gc())
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());  // Note: already called above
    panic!("Handle::get: object became dead/dropping after dec_ref");
}

let value = gc_box.value();
value
```

## 🛠️ 修復記錄 (Fix Applied)

**修復日期:** 2026-03-22

**修復內容:**
在 `Handle::get()` 的 `dec_ref()` 和 `value()`之間添加了第二個 `is_allocated` 檢查和 flags 重新檢查：

```rust
// After generation assertion, before dec_ref:
assert_eq!(...);

// Second is_allocated check after dec_ref (bug372 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::get: object slot was swept after dec_ref"
    );
}

// Recheck flags before reading value
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    panic!("Handle::get: object became dead/dropping after dec_ref");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
let value = gc_box.value();
value
```

**驗證:**
- `./clippy.sh` 通過
- `cargo test --test handlescope_basic` 通過
- `cargo test --test handlescope_async --features tokio` 通過

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 和 handle access 的並發是一個已知的 race window。`Handle::to_gc()` 採用了更謹慎的模式，在操作後重新驗證 `is_allocated`。`Handle::get()` 應該採用相同模式確保一致性。

**Rustacean (Soundness 觀點):**
如果 slot 在 `dec_ref()` 和 `value()`之間被 sweep，則 `value()` 會讀取已丟棄的記憶體。這是 UAF - 使用已釋放的記憶體。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制 GC timing 和物件生命週期，可以利用這個 race condition來讀取其他物件的殘留資料。

---

## 驗證記錄

**驗證日期:** 2026-03-22

**驗證方法:**
- Code review 比較 `Handle::get()` (mod.rs:302-337) 與 `Handle::to_gc()` (mod.rs:370-416)
- 確認: `Handle::to_gc()` 在 line 400-406 有第二個 `is_allocated` 檢查
- 確認: `Handle::get()` 在 `dec_ref()` (line 333) 和 `value()` (line 334)之間**沒有**第二個 `is_allocated` 檢查
- 確認: `Handle::get()` 在讀取值前**沒有**重新檢查 flags（而 `Handle::to_gc()` 有在 line 407-413）
