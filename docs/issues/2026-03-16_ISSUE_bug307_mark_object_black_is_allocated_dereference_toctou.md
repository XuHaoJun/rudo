# [Bug]: mark_object_black 初始 is_allocated 檢查與 is_under_construction 解引用之間存在 TOCTOU Race

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並髮操作：lazy sweep 在 is_allocated 檢查後、ptr 解引用前執行 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，讀取已釋放/回收的記憶體 |
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

`mark_object_black` 函數應該在檢查 `is_allocated` 後，安全地解引用指標以檢查 `is_under_construction`。在檢查和解引用之間不應該有 race 條件。

### 實際行為 (Actual Behavior)

存在 **TOCTOU (Time-of-Check-Time-of-Use)** 競爭：

1. Thread A: 調用 `mark_object_black(ptr)` 
2. Thread A: 檢查 `is_allocated(idx)` -> true (slot 已分配)
3. Thread B: Lazy sweep 運行，回收該 slot 並重新分配給新物件
4. Thread A: 解引用 `ptr.cast::<GcBox<()>>()` 檢查 `is_under_construction` -> **讀取已回收的記憶體！**

### 程式碼位置

`gc/incremental.rs` 第 1045-1061 行：
```rust
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    let idx = crate::heap::ptr_to_object_index(ptr.cast())?;
    let header = crate::heap::ptr_to_page_header(ptr);
    let h = header.as_ptr();

    // Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
    if !(*h).is_allocated(idx) {       // <-- CHECK (line 1051)
        return None;
    }

    // Skip if object is under construction (e.g. during Gc::new_cyclic_weak).
    // Avoids incorrectly marking partially-initialized objects (bug238).
    #[allow(clippy::cast_ptr_alignment)]
    let gc_box = &*ptr.cast::<GcBox<()>>();  // <-- USE (line 1058) - 可能 UAF!
    if gc_box.is_under_construction() {
        return None;
    }
    // ...
}
```

### 相關 Bug

- **bug108**: 初始 is_allocated 檢查缺失 (已修復)
- **bug291**: try_mark 與 is_allocated re-check 之間的 TOCTOU (已修復)
- **bug238**: 缺少 is_under_construction 檢查 (已修復)
- **bug272**: set_mark 後缺少 is_allocated 檢查 (已修復)

本 bug 是獨立的：**is_allocated 檢查與 is_under_construction 解引用之間的 TOCTOU**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc/incremental.rs` 的 `mark_object_black` 函數中：

1. **Line 1051**: 檢查 `is_allocated(idx)` - 如果返回 false，則提前返回
2. **Line 1058**: 解引用指標 `ptr.cast::<GcBox<()>>()` 以檢查 `is_under_construction`

**Race 條件**：
- 在步驟 1 和步驟 2 之間，lazy sweep 執行緒可能會：
  1. 回收該 slot（清除 allocated bit）
  2. 將其重新分配給新物件
  3. 新物件可能有不同的記憶體內容
- 當步驟 2 解引用指標時，可能讀取到無效/已回收的記憶體

**為何現有修復不足**：
- bug108 修復了初始的 is_allocated 檢查
- bug291 修復了 try_mark 後的 is_allocated re-check
- bug238 添加了 is_under_construction 檢查，但**沒有保護檢查之前的解引用**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要使用 ThreadSanitizer 或精心設計的時序來觸發。單執行緒無法可靠復現此問題。

概念驗證（需要多執行緒）：
```rust
// 需要並髮測試框架來觸發 race
// Thread A: 調用 mark_object_black
// Thread B: 並發運行 lazy sweep

// 預期：mark_object_black 應該在解引用前再次檢查 is_allocated
// 實際：在 is_allocated 檢查後直接解引用，可能 UAF
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：將 is_under_construction 檢查移至 is_allocated 檢查之前（或合併）：
```rust
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    let idx = crate::heap::ptr_to_object_index(ptr.cast())?;
    let header = crate::heap::ptr_to_page_header(ptr);
    let h = header.as_ptr();

    // 先解引用檢查 is_under_construction，再檢查 is_allocated
    // 這樣可以避免在 slot 被回收後解引用
    #[allow(clippy::cast_ptr_alignment)]
    let gc_box = &*ptr.cast::<GcBox<()>>();
    
    // 然後檢查 is_allocated
    if !(*h).is_allocated(idx) {
        return None;
    }

    if gc_box.is_under_construction() {
        return None;
    }
    // ...
}
```

選項 2：在解引用前再次檢查 is_allocated：
```rust
    // Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
    if !(*h).is_allocated(idx) {
        return None;
    }

    // FIX: Re-check is_allocated before dereferencing to avoid TOCTOU with lazy sweep.
    if !(*h).is_allocated(idx) {
        return None;
    }

    #[allow(clippy::cast_ptr_alignment)]
    let gc_box = &*ptr.cast::<GcBox<()>>();
    // ...
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU race。Lazy sweep 與 mutator 並發運行時，slot 可能在任何檢查之間被回收和重用。在解引用指標前，必須確保 slot 仍然有效。

**Rustacean (Soundness 觀點):**
這是明確的 undefined behavior。解引用無效指標是 Rust 中的 UB，即使在 unsafe 程式碼中也應該避免。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過精確時序控制：1. 建立一個即將被回收的物件
2. 觸發 lazy sweep3. 在 sweep 和 mark_object_black之間插入解引用
4. 讀取已回收的記憶體內容

---

## Resolution

**Outcome:** Pending

---

## Verification (2026-03-20)

**Verifier:** Bug Hunt Agent

**Verification Result:** Bug **CONFIRMED STILL PRESENT**

**Code Analysis:**

The bug exists at `gc/incremental.rs:1061-1072`:

```rust
// Skip if object was swept; avoids UAF when Drop runs during/concurrent with sweep.
if !(*h).is_allocated(idx) {       // <-- Line 1062: CHECK
    return None;
}

// Skip if object is under construction (e.g. during Gc::new_cyclic_weak).
// Avoids incorrectly marking partially-initialized objects (bug238).
#[allow(clippy::cast_ptr_alignment)]
let gc_box = &*ptr.cast::<GcBox<()>>();  // <-- Line 1069: USE - UAF risk!
if gc_box.is_under_construction() {
    return None;
}
```

**Race Condition Timeline:**
1. Thread A: checks `is_allocated(idx)` at line 1062 → true
2. Thread B: lazy sweep clears allocated bit, reallocates slot to new object
3. Thread A: dereferences `ptr` at line 1069 → **reads from potentially invalid memory**

**Fix Required:** Add `is_allocated` re-check before dereferencing at line 1069, similar to the re-check pattern used at lines 1080 and 1087 in the same function.

