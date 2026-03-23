# [Bug]: Handle::get() bug372 fix incorrectly applied - checks positioned BEFORE dec_ref instead of AFTER

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep during Handle::get() |
| **Severity (嚴重程度)** | `Critical` | Potential UAF - reading value from swept slot |
| **Reproducibility (重現難度)** | `Medium` | Requires precise thread timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::get()` in `handles/mod.rs:334-356`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

Bug372 的修復應該在 `dec_ref()` 和 `value()`之間添加 `is_allocated` 檢查和 flags 重新檢查，以防止 lazy sweep 在 dec_ref 和 value read 之間回收 slot。

### 實際行為 (Actual Behavior)

Bug372 的修復被錯誤地應用了。儘管註釋明確說「Second is_allocated check **after** dec_ref to fix TOCTOU with lazy sweep (bug372)」，但實際檢查位於 `dec_ref()` **之前**，而非之後。

**當前程式碼順序 (handles/mod.rs:334-356):**
```rust
// 註釋說 "after dec_ref" - 但檢查在 dec_ref 之前！
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {  // 337-343
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::get: object slot was swept after dec_ref"
    );
}

// Recheck flags before reading value (same as Handle::to_gc).
if gc_box.has_dead_flag()  // 347-352 - 也在 dec_ref 之前！
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    panic!("Handle::get: object became dead/dropping after dec_ref");
}

crate::GcBox::dec_ref(gc_box_ptr.cast_mut());  // 354 - dec_ref
let value = gc_box.value();  // 355 - value read - 中間沒有檢查！
value
```

**正確的順序應該是:**
```rust
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());

// Second is_allocated check AFTER dec_ref (bug372 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::get: object slot was swept after dec_ref"
    );
}

// Recheck flags after dec_ref before reading value
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    panic!("Handle::get: object became dead/dropping after dec_ref");
}

let value = gc_box.value();
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

Race Condition 詳細過程（正確順序下）：

1. Object A 存在於 slot，generation = 1, refcount = 2
2. Thread A 呼叫 `Handle::get()`
3. Thread A 通過第一個 `is_allocated` 檢查（line 310-315）
4. Thread A 通過 flags 檢查（line 318-323）
5. Thread A 保存 `pre_generation = 1`（line 324）
6. Thread A 呼叫 `try_inc_ref_if_nonzero()` - refcount 變為 3（line 325）
7. Thread A 執行 `assert_eq(1, 1)` - 通過（line 328-332）
8. Thread A 執行 `dec_ref()` - refcount 變為 2（line 354）
9. **Race Window**: Lazy sweep 在此時運行：
   - 確認 Object A 已死亡，回收 slot
   - 設置 `is_allocated = false`
   - 調用 `drop_fn` 丟棄 Object A 的值
10. Thread A 執行 `value()` - **讀取已丟棄的值！**（line 355）- **UAF!**

**為什麼當前程式碼有問題：**

當 dec_ref() 在 line 354 執行時，如果 refcount 降到 0 並觸發 deallocation/lazy sweep，slot 可能會在 dec_ref 和 value() read 之间被回收。但由於 is_allocated check (337-343) 和 flags recheck (347-352) 都已經在 dec_ref 之前執行過了，它們無法捕捉到這個 race。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確控制執行緒調度的並發測試環境
// 1. 建立 HandleScope 和 Handle
// 2. 減少 refcount 使物件符合 sweep 條件
// 3. 在 dec_ref() 和 value() 之间觸發 lazy sweep
// 4. 觀察: 是否讀取到已丟棄的值
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 is_allocated check 和 flags recheck 從 dec_ref **之前**移動到 dec_ref **之後**：

```rust
crate::GcBox::dec_ref(gc_box_ptr.cast_mut());

// Second is_allocated check AFTER dec_ref (bug372 fix)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::get: object slot was swept after dec_ref"
    );
}

// Recheck flags after dec_ref before reading value
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    panic!("Handle::get: object became dead/dropping after dec_ref");
}

let value = gc_box.value();
value
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 和 handle access 的並發是一個已知的 race window。如果 dec_ref() 觸發了 deallocation，slot 可能會在 dec_ref 和 value read 之間被回收並重用。在 dec_ref 之後再次檢查 is_allocated 是正確的防御。

**Rustacean (Soundness 觀點):**
如果 slot 在 `dec_ref()` 和 `value()`之間被 sweep，則 `value()` 會讀取已丟棄的記憶體。這是 UAF - 使用已釋放的記憶體。檢查應該在 dec_ref 之後驗證 slot 的有效性。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過精確時序控制來利用此漏洞。dec_ref() 和 value()言之間的窗口雖然很小，但並非不存在。如果攻擊者能觸發 GC timing，可能讀取到其他物件的殘留資料。

---

## 驗證記錄

**驗證日期:** 2026-03-24

**驗證方法:**
- Code review `Handle::get()` (mod.rs:302-358)
- 確認: 註釋在 line 334 說 "after dec_ref" 但檢查在 lines 337-352（dec_ref 之前）
- 對比 bug372 的建議修復顯示檢查應該在 dec_ref 之後
- 確認這是 bug372 修復的錯誤應用，而非新的 bug