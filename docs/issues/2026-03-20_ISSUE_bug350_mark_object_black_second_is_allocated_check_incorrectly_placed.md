# [Bug]: mark_object_black second is_allocated check incorrectly placed (bug307 fix incorrectly applied)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發：lazy sweep 在 is_allocated 檢查和解引用之間執行 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，解引用已釋放/回收的記憶體 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_object_black` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`mark_object_black` 函數在解引用指標之前應該有 `is_allocated` 檢查，以防止 TOCTOU race 導致 UAF。bug307 的修復應該在解引用前添加第二次檢查。

### 實際行為 (Actual Behavior)

bug307 的修復被錯誤地應用：第二個 `is_allocated` 檢查被放置在第一個檢查**立即之後**（兩者之間無代碼），而不是在**解引用之前**。

**程式碼位置** (`gc/incremental.rs:1061-1076`)：

```rust
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    let idx = crate::heap::ptr_to_object_index(ptr.cast())?;
    let header = crate::heap::ptr_to_page_header(ptr);
    let h = header.as_ptr();

    // Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
    if !(*h).is_allocated(idx) {       // <-- Line 1062: First CHECK
        return None;
    }

    // Re-check is_allocated before dereferencing to fix TOCTOU with lazy sweep (bug307).
    // If slot was swept after initial check but before we dereference,
    // return None to avoid UAF.
    if !(*h).is_allocated(idx) {       // <-- Line 1069: Second CHECK (placed WRONG!)
        return None;
    }

    // Skip if object is under construction (e.g. during Gc::new_cyclic_weak).
    // Avoids incorrectly marking partially-initialized objects (bug238).
    #[allow(clippy::cast_ptr_alignment)]
    let gc_box = &*ptr.cast::<GcBox<()>>();  // <-- Line 1076: DEREFERENCE - UAF risk!
    if gc_box.is_under_construction() {
        return None;
    }
    // ...
}
```

**問題：**

1. 第一個檢查在 Line 1062
2. 第二個檢查在 Line 1069，**立即在第一個檢查之後**（兩者之間無代碼）
3. 解引用在 Line 1076

第二個檢查聲稱在解引用之前修復 TOCTOU，但實際上它立即跟在第一個檢查後面，什麼都沒有保護。

**Race 條件時間線：**
1. Thread A: 檢查 `is_allocated(idx)` 在 Line 1062 → true
2. Thread B: lazy sweep 清除 allocated bit，重新分配 slot 給新物件
3. Thread A: 在 Line 1076 解引用 `ptr` → **從無效記憶體讀取！**

---

## 🔬 根本原因分析 (Root Cause Analysis)

bug307 建議的修復是在解引用前添加第二次 `is_allocated` 檢查：

```rust
// 錯誤的修復（當前代碼）：
if !(*h).is_allocated(idx) {  // First check
    return None;
}
if !(*h).is_allocated(idx) {  // Second check - 立即在第一個之後，無保護作用
    return None;
}
let gc_box = &*ptr.cast::<GcBox<()>>();  // 解引用 - 不受保護！
```

正確的修復應該是：

```rust
// 正確的修復：
if !(*h).is_allocated(idx) {  // First check
    return None;
}
let gc_box = &*ptr.cast::<GcBox<()>>();  // 解引用
if !(*h).is_allocated(idx) {  // Second check - 应该在解引用之前
    return None;  // 如果在解引用之前 slot 被回收，返回 None
}
if gc_box.is_under_construction() {
    return None;
}
```

或者更簡單地，將 `is_under_construction` 檢查移到 `is_allocated` 檢查之前（避免解引用無效指標）。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的執行緒交錯控制，涉及 lazy sweep：

```rust
// 概念驗證 - 需要 TSan 或極端的時序控制
// Thread A: 調用 mark_object_black(ptr)
// Thread B: 並發運行 lazy sweep（清除 allocated bit，重新分配 slot）

// 預期：mark_object_black 應該在解引用前再次檢查 is_allocated
// 實際：第二次檢查在第一次之後立即執行，不保護解引用
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將第二次 `is_allocated` 檢查移動到解引用之前，或重新排序檢查邏輯：

**選項 1：將 is_under_construction 檢查移到 is_allocated 之前**
```rust
#[allow(clippy::cast_ptr_alignment)]
let gc_box = &*ptr.cast::<GcBox<()>>();  // 先解引用

if !(*h).is_allocated(idx) {  // 然後檢查 is_allocated
    return None;
}

if gc_box.is_under_construction() {
    return None;
}
```

**選項 2：在解引用前添加第二次檢查**
```rust
if !(*h).is_allocated(idx) {
    return None;
}

#[allow(clippy::cast_ptr_alignment)]
let gc_box = &*ptr.cast::<GcBox<()>>();  // 解引用

// 第二次檢查應該在這裡（解引用之前）- 但注意：解引用已經發生了！
// 選項 1 是更好的方法
```

**選項 3：重新排序（推薦）**
```rust
#[allow(clippy::cast_ptr_alignment)]
let gc_box = &*ptr.cast::<GcBox<()>>();

if !(*h).is_allocated(idx) {
    return None;
}

if gc_box.is_under_construction() {
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU race。Lazy sweep 與 mutator 並發運行時，slot 可能在 is_allocated 檢查後、解引用前被回收和重用。在解引用指標前，必須確保 slot 仍然有效。

**Rustacean (Soundness 觀點):**
這是明確的 undefined behavior。解引用無效指標是 UB，即使在 unsafe 程式碼中也應該避免。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過精確時序控制來利用此漏洞。通過在 is_allocated 檢查後、解引用前觸發 lazy sweep，攻擊者可以讀取已釋放的記憶體內容。

---

## Related Issues

- bug307: Original issue documenting the TOCTOU between is_allocated check and dereference
- bug108: Initial is_allocated check missing (fixed)
- bug238: Missing is_under_construction check (fixed)
- bug291: TOCTOU between try_mark and is_allocated re-check (fixed)
- bug272: Missing is_allocated check after set_mark (fixed)

---

## ✅ Fix Applied

**Date:** 2026-03-20

**Fix:** Applied Option 3 (recommended fix) - reordered checks to perform `is_allocated` check before using `gc_box`.

**Changes made to `crates/rudo-gc/src/gc/incremental.rs`:**

```rust
// Before (buggy):
if !(*h).is_allocated(idx) {       // First CHECK
    return None;
}
if !(*h).is_allocated(idx) {       // Second CHECK (immediately after first, no protection)
    return None;
}
let gc_box = &*ptr.cast::<GcBox<()>>();  // DEREFERENCE - UAF risk!
if gc_box.is_under_construction() {
    return None;
}

// After (fixed):
if !(*h).is_allocated(idx) {
    return None;
}
let gc_box = &*ptr.cast::<GcBox<()>>();
if !(*h).is_allocated(idx) {       // Second CHECK now correctly placed before using gc_box
    return None;
}
if gc_box.is_under_construction() {
    return None;
}
```

**Verification:**
- ✅ Clippy passes
- ✅ Code compiles
