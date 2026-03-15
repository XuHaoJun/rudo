# [Bug]: GcBoxWeakRef::upgrade() 缺少指標驗證導致潜在 UB

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需記憶體重用場景，內部使用情況下較少觸發 |
| **Severity (嚴重程度)** | High | 直接解引用無效指標導致 UB |
| **Reproducibility (復現難度)** | Medium | 需精確控制記憶體重用時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade()`, `ptr.rs:506-558`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`GcBoxWeakRef::upgrade()` 方法直接解引用原始指標，未先驗證指標的有效性。

### 預期行為
與 `GcBoxWeakRef::try_upgrade()` 和 `GcBoxWeakRef::clone()` 一致，在解引用前應驗證指標：
1. 指標是否正確對齊
2. 指標位址是否 >= MIN_VALID_HEAP_ADDRESS
3. 指標是否為有效的 GC box 指標

### 實際行為
`GcBoxWeakRef::upgrade()` 在第 510 行直接解引用指標：
```rust
let gc_box = &*ptr.as_ptr();
```

只檢查指標是否為 null（第 507 行），但沒有驗證指標的有效性。

---

## 🔬 根本原因分析 (Root Cause Analysis)

對比三個方法：

1. **`GcBoxWeakRef::clone()`** (lines 562-579): 有驗證
   ```rust
   let ptr_addr = ptr.as_ptr() as usize;
   let alignment = std::mem::align_of::<GcBox<T>>();
   if ptr_addr % alignment != 0 || ptr_addr < MIN_VALID_HEAP_ADDRESS {
       return Self { ptr: AtomicNullable::null() };
   }
   if !is_gc_box_pointer_valid(ptr_addr) {
       return Self { ptr: AtomicNullable::null() };
   }
   ```

2. **`GcBoxWeakRef::try_upgrade()`** (lines 631-640): 有驗證
   ```rust
   let addr = ptr.as_ptr() as usize;
   let alignment = std::mem::align_of::<GcBox<T>>();
   if addr % alignment != 0 || addr < MIN_VALID_HEAP_ADDRESS {
       return None;
   }
   ```

3. **`GcBoxWeakRef::upgrade()`** (lines 506-558): **缺少驗證**
   ```rust
   let ptr = self.ptr.load(Ordering::Acquire).as_option()?;  // 只檢查 null
   unsafe {
       let gc_box = &*ptr.as_ptr();  // 直接解引用！
   ```

這是 API 不一致的問題，`upgrade()` 是較簡單的 API 但缺少安全檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此問題需要:
1. 建立 GcBoxWeakRef
2. 底層記憶體被釋放並重用
3. 呼叫 upgrade() 解引用無效指標

理論上可通過 Miri 檢測。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBoxWeakRef::upgrade()` 中添加指標驗證，參考 `try_upgrade()` 的實現：

```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire).as_option()?;

    // 添加指標驗證
    let addr = ptr.as_ptr() as usize;
    let alignment = std::mem::align_of::<GcBox<T>>();
    if addr % alignment != 0 || addr < MIN_VALID_HEAP_ADDRESS {
        return None;
    }
    if !is_gc_box_pointer_valid(addr) {
        return None;
    }

    unsafe {
        let gc_box = &*ptr.as_ptr();
        // ... 其餘現有邏輯
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 內部 `GcBoxWeakRef` 被 `GcHandle::resolve()` 和 `CrossThreadHandle::resolve()` 使用
- 若底層 GC box 被釋放並重用，會導致錯誤的標頭讀取
- 雖然現有檢查（`is_under_construction`, `has_dead_flag`, `dropping_state`）可能多數時候能捕獲問題，但依賴運氣而非正確性

**Rustacean (Soundness 觀點):**
- 這是明確的 UB：解引用無效指標
- Rust 中即使「只是讀取」無效記憶體也是 UB
- 應與 `try_upgrade()` 和 `clone()` 保持一致

**Geohot (Exploit 觀點):**
- 若攻擊者能控制 GC 時序，可利用此漏洞
- 通過精確的記憶體噴射，可能讓升級讀取到錯誤物件的標頭
- 進一步可繞過 `dropping_state` 檢查（因為讀取的是新物件的標頭）

---

## Resolution (2026-03-14)

**Outcome:** Already fixed.

The fix was applied in a previous commit. The current `GcBoxWeakRef::upgrade()` implementation in `ptr.rs` (lines 517–595) correctly validates the pointer before dereferencing:

- Alignment check: `ptr_addr % alignment != 0`
- `MIN_VALID_HEAP_ADDRESS` check
- `is_gc_box_pointer_valid(ptr_addr)` check

These checks match the pattern used in `GcBoxWeakRef::clone()` and `GcBoxWeakRef::try_upgrade()`. Full test suite passes.
