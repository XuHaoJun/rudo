# [Bug]: AsyncHandle::get / get_unchecked Missing Second is_allocated Check After Generation Assertion

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep during AsyncHandle::get() |
| **Severity (嚴重程度)** | Critical | Potential UAF - reading value from swept slot |
| **Reproducibility (重現難度)** | Medium | Requires precise thread timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()` and `AsyncHandle::get_unchecked()` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 應該在讀取值之前驗證 slot 仍然處於 allocated 狀態。`Handle::get()` 已經有這個檢查（bug372 fix），`AsyncHandle::to_gc()` 也有這個檢查，但 `AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 缺少這個檢查。

### 實際行為 (Actual Behavior)

`AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 在 generation assertion 和 `value()`之間缺少第二個 `is_allocated` 檢查。

**`AsyncHandle::to_gc()` 有完整的檢查鏈:**
```rust
// 1. First is_allocated check (pre) - Line 778-785
// ... flags and generation checks ...
// ... try_inc_ref_if_nonzero() ...
// ... generation assertion ...

// 2. Second is_allocated check (POST-increment) - Line 803-809
if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    let header = crate::heap::ptr_to_page_header(...);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "AsyncHandle::to_gc: object slot was swept after inc_ref"
    );
}

// 3. Flags recheck - Line 810-816
Gc::from_raw(gc_box_ptr as *const u8)  // Safe to use
```

**`AsyncHandle::get()` 缺少第二個檢查:**
```rust
// 1. First is_allocated check (pre) - Line 620-626
if let Some(idx) = crate::heap::ptr_to_object_index(...) {
    let header = crate::heap::ptr_to_page_header(...);
    assert!(...);  // "AsyncHandle::get: slot has been swept and reused"
}
// ... flags and generation checks ...
// ... try_inc_ref_if_nonzero() ...
// ... generation assertion ... - Line 638-642

// 2. NO second is_allocated check! - MISSING!

// 3. dec_ref() - Line 643
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());

// 4. value() - Line 644 - BUG: No is_allocated check before reading value!
let value = gc_box.value();
```

**`AsyncHandle::get_unchecked()` 缺少第二個檢查:**
```rust
// 1. First is_allocated check (pre) - Line 694-700
// ... flags and generation checks ...
// ... generation assertion ... - Line 712-716

// 2. NO second is_allocated check! - MISSING!

// 3. dec_ref() - Line 717
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());

// 4. value() - Line 718 - BUG: No is_allocated check before reading value!
let value = gc_box.value();
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

Race Condition 詳細過程：

1. Object A 存在於 slot，generation = 1, refcount = 1
2. Thread A 呼叫 `AsyncHandle::get()`
3. Thread A 通過第一個 `is_allocated` 檢查（line 620-626）
4. Thread A 通過 flags 檢查（line 628-633）
5. Thread A 保存 `pre_generation = 1`（line 634）
6. Thread A 呼叫 `try_inc_ref_if_nonzero()` - refcount 變為 2（line 635）
7. Thread A 執行 `assert_eq(1, 1)` - 通過（line 638-642）
8. **Race Window**: Lazy sweep 在此時運行:
   - 確認 Object A 已死亡，回收 slot
   - 設置 `is_allocated = false`
   - 調用 `drop_fn` 丟棄 Object A 的值
   - **但 slot 未被重用**，generation 保持為 1
9. Thread A 執行 `dec_ref()` - refcount 變為 1（line 643）
10. Thread A 執行 `value()` - **讀取已丟棄的值！**（line 644）- **UAF!**

Generation 檢查無法捕獲此問題，因為：
- Generation 只在 slot **重用** 時改變
- 如果 slot 被 sweep 後**未立即重用**，generation 保持不變
- Generation 檢查只驗證 slot 是否被重用，不驗證 slot 是否仍然 allocated

此問題與 bug372（Handle::get 缺少第二個 is_allocated 檢查）為同一模式，但發生在 AsyncHandle。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確控制執行緒調度的並發測試環境
// 1. 建立 AsyncHandleScope 和 AsyncHandle
// 2. 減少 refcount 到 1（使物件符合 sweep 條件）
// 3. 在另一執行緒觸發 lazy sweep 回收 slot（但不立即重用）
// 4. 同時呼叫 AsyncHandle::get()
// 5. 觀察: 是否讀取到已丟棄的值
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 的 `dec_ref()` 和 `value()`之間添加第二個 `is_allocated` 檢查，並在讀取值之前重新檢查 flags：

**AsyncHandle::get() 修復:**
```rust
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "AsyncHandle::get: slot was reused before value read (generation mismatch)"
);

// Second is_allocated check after dec_ref (bug377 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "AsyncHandle::get: object slot was swept after dec_ref"
    );
}

// Recheck flags before reading value (same as Handle::get)
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: object became dead/dropping after dec_ref");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
let value = gc_box.value();
value
```

**AsyncHandle::get_unchecked() 修復:**
```rust
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "AsyncHandle::get_unchecked: slot was reused before value read (generation mismatch)"
);

// Second is_allocated check after dec_ref (bug377 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "AsyncHandle::get_unchecked: object slot was swept after dec_ref"
    );
}

// Recheck flags before reading value
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get_unchecked: object became dead/dropping after dec_ref");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
let value = gc_box.value();
value
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 和 async handle access 的並發是一個已知的 race window。`Handle::get()` 和 `AsyncHandle::to_gc()` 都採用了更謹慎的模式，在操作後重新驗證 `is_allocated`。`AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 應該採用相同模式確保一致性。

**Rustacean (Soundness 觀點):**
如果 slot 在 `dec_ref()` 和 `value()`之間被 sweep（但未重用），則 `value()` 會讀取已丟棄的記憶體。這是 UAF - 使用已釋放的記憶體。Generation 檢查只對 slot 重用有效，對純 sweep 無效。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制 GC timing 和物件生命週期，可以利用這個 race condition 來讀取其他物件的殘留資料。特別是如果 lazy sweep 在某個時間點回收 slot 但延遲重用，攻擊者可能有機會讀取已釋放物件的記憶體。

---

## 🔗 相關 Issue

- bug372: Handle::get 缺少第二個 is_allocated 檢查（同一模式）
- bug196: AsyncHandle::get / to_gc 缺少第一個 is_allocated 檢查（不同 - 第一次檢查）

---

## 🛠️ 修復記錄 (Fix Applied)

**修復日期:** 2026-03-23

**修復內容:**
在 `AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 的 `dec_ref()` 和 `value()`之間添加了第二個 `is_allocated` 檢查和 flags 重新檢查：

**AsyncHandle::get() 修復 (async.rs:638-665):**
```rust
assert_eq!(...generation...);

// Second is_allocated check (bug377 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "AsyncHandle::get: object slot was swept after dec_ref"
    );
}

// Recheck flags before reading value
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: object became dead/dropping after dec_ref");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
let value = gc_box.value();
value
```

**AsyncHandle::get_unchecked() 修復 (async.rs:735-752):**
```rust
assert_eq!(...generation...);

// Second is_allocated check (bug377 fix)
if let Some(idx) = unsafe { crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) } {
    let header = unsafe { crate::heap::ptr_to_page_header(gc_box_ptr as *const u8) };
    assert!(
        unsafe { (*header.as_ptr()).is_allocated(idx) },
        "AsyncHandle::get_unchecked: object slot was swept after dec_ref"
    );
}

// Recheck flags before reading value
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get_unchecked: object became dead/dropping after dec_ref");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());
let value = gc_box.value();
value
```

**驗證:**
- `./clippy.sh` 通過
- `./test.sh` 通過 (所有測試)