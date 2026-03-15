# [Bug]: Weak::upgrade 和 Weak::try_upgrade 缺少升級後的 is_allocated 檢查

**Status:** Fixed
**Tags:** Verified

---

## 📊 Threat Model Assessment

| Aspect | Assessment |
|--------|------------|
| Likelihood | Medium |
| Severity | High |
| Reproducibility | Medium |

---

## 🧩 Affected Component & Environment

- **Component:** `Weak::upgrade()` (ptr.rs:1854-1914) 和 `Weak::try_upgrade()` (ptr.rs:1933-1999)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 Description

### Expected Behavior

`Weak::upgrade()` 和 `Weak::try_upgrade()` 應該在成功升級後執行 `is_allocated` 檢查，以防止 lazy sweep 回收並重新分配插槽後，返回指向錯誤物件的 Gc。這與 `GcBoxWeakRef::upgrade()` 的行為一致。

### Actual Behavior

`Weak::upgrade()` (ptr.rs:1900-1910) 和 `Weak::try_upgrade()` (ptr.rs:1985-1995) 在 CAS 成功後只檢查 `dropping_state()` 和 `has_dead_flag()`，**沒有檢查 `is_allocated()`**。

相比之下，`GcBoxWeakRef::upgrade()` (ptr.rs:583-590) 正確地包含此檢查：

```rust
// Check is_allocated after successful upgrade to prevent slot reuse issues
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
}
```

---

## 🔬 Root Cause Analysis

當 Weak 指標存儲在可能比 GC 物件壽命更長的資料結構中，且 lazy sweep 並發執行時：

1. 插槽中的物件 A 被 lazy sweep 回收（釋放）
2. 物件 B 被分配到同一個插槽
3.  Mutator 對物件 B 的 Weak 調用 `upgrade()` 或 `try_upgrade()`
4. 舊指標（現在指向物件 B 的插槽）通過所有旗標檢查
5. 解引用該插槽 - 但裡面是物件 B 的資料！
6. 返回指向錯誤物件的 Gc 或讀取無效記憶體

---

## 💣 Steps to Reproduce / PoC

```rust
// Requires concurrent test environment:
// 1. Store Weak in a data structure that outlives the GC object
// 2. Trigger lazy sweep to reclaim original object
// 3. Allocate new object in same slot
// 4. Call Weak::upgrade() or Weak::try_upgrade()
// 5. Observe incorrect behavior (wrong object or invalid memory access)
```

---

## 🛠️ Suggested Fix / Remediation

在 `Weak::upgrade()` 和 `Weak::try_upgrade()` 的 CAS 成功後添加 `is_allocated` 檢查，與 `GcBoxWeakRef::upgrade()` 的模式一致：

```rust
// 在 ptr.rs:1900 (Weak::upgrade) 和 ptr.rs:1985 (Weak::try_upgrade) 的 post-CAS 檢查後添加：

// Check is_allocated after successful upgrade to prevent slot reuse issues
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
}
```

---

## 🗣️ Internal Discussion Record

### R. Kent Dybvig
這與 `GcBoxWeakRef::upgrade()` 的修復為同一模式。lazy sweep 的並發性質使得在 CAS 成功後檢查插槽是否仍然分配變得必要。

### Rustacean
缺少的驗證可能導致讀取無效記憶體或返回指向錯誤物件的 Gc。雖然隨後的旗標檢查（has_dead_flag、dropping_state）提供了一些保護，但它們無法捕捉 lazy sweep 插槽重複使用的所有情況。

### Geohot
攻擊者控制 GC 時機可以觸發精確的插槽重用，使 upgrade() 返回指向錯誤物件的 Gc，從而可能進行進一步利用。

---

## Resolution (2026-03-14)

**Outcome:** Already fixed.

The fix was applied in a prior commit. The current `Weak::upgrade()` and `Weak::try_upgrade()` implementations in `ptr.rs` (lines 2016–2092 and 2111–2197) correctly include `is_allocated` checks in both places:

1. **Entry check** (before CAS loop): Lines 2019–2026 (upgrade), 2131–2138 (try_upgrade)
2. **Post-CAS check** (after successful upgrade): Lines 2076–2083 (upgrade), 2181–2187 (try_upgrade)

Behavior now matches `GcBoxWeakRef::upgrade()` as described in the issue. Verified via weak.rs and weak_memory_reclaim.rs tests.
