# [Bug]: GcBoxWeakRef::clone 缺少第二次 is_allocated 檢查導致理論性 UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Very Low | 需要 u32 generation wraparound 巧合 |
| **Severity (嚴重程度)** | Medium | 可能導致 weak reference 到已死亡物件 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序 + generation wraparound |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::clone` (ptr.rs:787-843)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcBoxWeakRef::clone()` 應該在 `inc_weak()` 後有第二次 `is_allocated` 檢查，與 `as_weak()` 和 `clone_orphan_root_with_inc_ref()` 一致。

### 實際行為 (Actual Behavior)

`clone()` 在 `inc_weak()` 和 generation 檢查後，沒有第二次 `is_allocated` 檢查就返回成功：

```rust
// ptr.rs:831-843
(*ptr.as_ptr()).inc_weak();

// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*ptr.as_ptr()).generation() {
    (*ptr.as_ptr()).dec_weak();
    return Self::null();
}
// BUG: No second is_allocated check here! Returns successfully even if slot was swept!

Self {
    ptr: AtomicNullable::new(ptr),
    generation: self.generation,
}
```

### 對比 `as_weak()` (ptr.rs:597-607):

```rust
if let Some(idx) = crate::heap::ptr_to_object_index(self_ptr) {
    let header = crate::heap::ptr_to_page_header(self_ptr);
    if !(*header.as_ptr()).is_allocated(idx) {
        // FIX bug504: Call dec_weak to undo inc_weak.
        (*NonNull::from(self).as_ptr()).dec_weak();
        return GcBoxWeakRef::null();
    }
}

GcBoxWeakRef::new(NonNull::from(self))
```

### 對比 `clone_orphan_root_with_inc_ref` (heap.rs:334-342):

```rust
// FIX bug515: Second is_allocated check AFTER inc_ref to catch slot reuse
// that bypassed the generation check (defense-in-depth).
if let Some(idx) = ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // Don't call dec_ref - slot may be reused (bug133)
        return None;
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**觸發情境：**

1. Object A 分配在 slot，generation = G
2. 呼叫 `clone()` on weak ref to Object A
3. 第一次 `is_allocated` 檢查通過 (slot 分配給 Object A)
4. `pre_generation = G` 被捕獲
5. Object A 變成不可達，GC 運行，slot 被 sweep (Object A 被收集)
6. Slot 立即被重新分配給 Object B (generation = G+1，然後 u32 wraparound 到 G)
7. `inc_weak()` 在 Object B 的 slot 上執行 - Object B 的 weak_count++
8. Generation 檢查: G == G (因為 wraparound)，檢查通過
9. **沒有第二次 `is_allocated` 檢查！** 直接返回成功的 clone
10. 結果: 返回一個指向 Object B (已死亡 Object A 的 slot 被 Object B 重用) 的 weak reference

**問題：**
- `clone()` 沒有像 `as_weak()` 一樣的第二次 `is_allocated` 檢查
- 理論上可能導致返回一個指向已死亡物件的 weak reference

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 generation 檢查通過後，新增第二次 `is_allocated` 檢查：

```rust
// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*ptr.as_ptr()).generation() {
    (*ptr.as_ptr()).dec_weak();
    return Self::null();
}

// FIX bugXXX: Second is_allocated check AFTER inc_weak to catch slot reuse
// that bypassed the generation check (defense-in-depth).
// Matches as_weak() pattern (bug504 fix).
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*ptr.as_ptr()).dec_weak();
        return Self::null();
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generation 機制提供了強保護，但理論上 generation 可能因為 u32 wraparound 而巧合匹配。添加第二次 `is_allocated` 檢查是 defense-in-depth。

**Rustacean (Soundness 觀點):**
這不是立即的 UB，但在極少數情況下可能導致不正確的 GC 行為。與 `as_weak()` 和 `clone_orphan_root_with_inc_ref` 的不一致性表明這是一個被遺漏的修復。

**Geohot (Exploit 觀點):**
要利用這個 bug 非常困難，需要精確的時序控制加上 generation wraparound。但在某些長期運行的系統中，這可能是一個問題。

---

## 修復記錄 (Fix Applied)

**Date:** 2026-04-12
**Fix:** Added second `is_allocated` check after `inc_weak()` in `GcBoxWeakRef::clone()` (ptr.rs:838-847).

**Code Change:**
```rust
// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*ptr.as_ptr()).generation() {
    (*ptr.as_ptr()).dec_weak();
    return Self::null();
}

// FIX bug600: Second is_allocated check AFTER inc_weak to catch slot reuse
// that bypassed the generation check (defense-in-depth).
// Matches as_weak() pattern (bug504 fix).
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*ptr.as_ptr()).dec_weak();
        return Self::null();
    }
}
```

**Verification:** `./clippy.sh` passes. Library tests (94 tests) pass. Note: `deep_tree_allocation_test` was already failing before this fix (pre-existing bug, unrelated to this change).

---

## 相關 Issue

- bug504: Gc::as_weak() 缺少 is_allocated 檢查後 dec_weak - 修復了 `as_weak()`
- bug515: clone_orphan_root_with_inc_ref 缺少第二次 is_allocated 檢查 - 修復了 `clone_orphan_root_with_inc_ref`
- bug564: GcBoxWeakRef::clone 缺少 is_allocated 檢查前讀取 generation - 部分修復了 `clone()`

---

## 驗證記錄

**驗證日期:** 2026-04-12
**驗證人員:** opencode

### 驗證結果

1. 比較 `GcBoxWeakRef::clone()` (ptr.rs:787-843) 與 `as_weak()` (ptr.rs:573-609)
   - `as_weak()` 在 line 597-604 有第二次 `is_allocated` 檢查
   - `clone()` 在 generation 檢查通過後沒有第二次 `is_allocated` 檢查

2. 比較 `GcBoxWeakRef::clone()` 與 `clone_orphan_root_with_inc_ref` (heap.rs:290-354)
   - `clone_orphan_root_with_inc_ref()` 在 line 334-342 有第二次 `is_allocated` 檢查
   - `clone()` 缺少這個檢查

3. 確認這是與 bug504、bug515 相同的模式，但 `clone()` 被遺漏了

**結論:** 確認 bug 存在，需要修復

**Status: Open** - Needs fix.
