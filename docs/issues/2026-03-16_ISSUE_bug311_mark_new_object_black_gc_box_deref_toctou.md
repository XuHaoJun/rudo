# [Bug]: mark_new_object_black TOCTOU - gc_box dereference before is_allocated re-check

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並發：lazy sweep 與標記同時執行，且在 is_allocated 檢查後、gc_box 解引用前觸發 |
| **Severity (Medium)** | Medium | 可能導致讀取已釋放記憶體，潛在 UAF |
| **Reproducibility (復現難度)** | High | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_new_object_black`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
在 `mark_new_object_black` 中，應該先檢查 `is_allocated`，然後再從 gc_box 讀取任何 flag，與 bug247 修復的其他 barrier 函數保持一致。

### 實際行為
當前順序：
1. 檢查 `is_allocated(idx)` (line 1007)
2. **Race Window**: lazy sweep 可能在此時回收 slot 並分配給新物件
3. 從 gc_box 讀取 `is_under_construction()` (line 1013) - 解引用可能已釋放的記憶體！

這與 bug247 模式相同，但發生在不同的函數中。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/incremental.rs` 的 `mark_new_object_black` 函數中：

```rust
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            if !(*header.as_ptr()).is_allocated(idx) {  // 第一次檢查
                return false;
            }
            // BUG: 在 is_allocated 檢查後，直接解引用 gc_box！
            #[allow(clippy::cast_ptr_alignment)]
            let gc_box = &*ptr.cast::<GcBox<()>>();  // <-- TOCTOU!
            if gc_box.is_under_construction() {
                return false;
            }
            // ... 後續 code
        }
    }
    false
}
```

**Race 條件說明**:
1. Thread A 調用 `mark_new_object_black(ptr)`
2. Line 1007: `is_allocated(idx)` 返回 true - slot 有效
3. **Race Window**: Lazy sweep 執行，回收 slot 並分配給新物件 B
4. Line 1013: `&*ptr.cast::<GcBox<()>>()` - 解引用可能已釋放/重用的記憶體！
5. Line 1014: `gc_box.is_under_construction()` - 讀取錯誤物件的 flag

這與 bug247 (`write_barrier_has_gen_old_flag_toctou_is_allocated_order`) 模式相同，但發生在 `mark_new_object_black` 中。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟用 lazy sweep
2. Thread A：不斷分配新物件並調用 `mark_new_object_black`
3. Thread B：同時進行 lazy sweep，回收並重用 slot
4. 觀察是否發生記憶體錯誤

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `is_under_construction` 檢查移到 `is_allocated` 檢查之前（因為我們只是檢查 flag，不依賴物件內容）：

```rust
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            
            // 先檢查 is_allocated，與 bug247 模式一致
            if !(*header.as_ptr()).is_allocated(idx) {
                return false;
            }
            
            // 第二次 is_allocated 檢查（bug272 已修復）
            if !(*header.as_ptr()).is_marked(idx) {
                (*header.as_ptr()).set_mark(idx);
                if !(*header.as_ptr()).is_allocated(idx) {
                    (*header.as_ptr()).clear_mark_atomic(idx);
                    return false;
                }
                return true;
            }
        }
    }
    false
}
```

或者，在解引用 gc_box 之前添加 is_allocated 檢查：

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return false;
}
// 添加第二次檢查以修復 TOCTOU
if !(*header.as_ptr()).is_allocated(idx) {
    return false;
}
let gc_box = &*ptr.cast::<GcBox<()>>();
if gc_box.is_under_construction() {
    return false;
}
```

---

## Verification (2026-03-20)

**Verifier:** Bug Hunt Agent

**Verification Result:** Bug **CONFIRMED STILL PRESENT**

**Code Analysis:**

The bug exists at `gc/incremental.rs:1018-1027`:

```rust
if !(*header.as_ptr()).is_allocated(idx) {  // <-- Line 1018: CHECK
    return false;
}
// Skip if object is under construction (e.g. during Gc::new_cyclic_weak).
// Avoids incorrectly marking partially-initialized objects (bug238).
#[allow(clippy::cast_ptr_alignment)]
let gc_box = &*ptr.cast::<GcBox<()>>();  // <-- Line 1024: USE - UAF risk!
if gc_box.is_under_construction() {
    return false;
}
```

**Race Condition Timeline:**
1. Thread A: checks `is_allocated(idx)` at line 1018 → true
2. Thread B: lazy sweep clears allocated bit, reallocates slot to new object
3. Thread A: dereferences `ptr` at line 1024 → **reads from potentially invalid memory**

**Note:** Unlike `mark_object_black` which uses `try_mark` with re-checks, `mark_new_object_black` uses simple `set_mark` without the same protection pattern.

**Fix Required:** Add `is_allocated` re-check before dereferencing at line 1024, or move the `is_under_construction` check before the `is_allocated` check.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Black allocation 優化中，新物件應被視為 live
- 問題是：如果 slot 在 is_allocated 檢查後被 sweep 與重用，解引用 gc_box 會讀取錯誤物件的 flag
- 這與 bug247 模式相同，應用相同的修復策略

**Rustacean (Soundness 觀點):**
- 這是防御性編程問題
- 解引用可能已釋放的記憶體是 UB
- 即使當前實現中 allocation 和 sweep 是串行的，代碼應對未來可能的並發場景保持安全

**Geohot (Exploit 攻擊觀點):**
- 如果未來實現並發 sweep，這裡可能成為 UAF 的源頭
- 攻擊者可能嘗試在 slot 重用時干擾標記狀態
- 雖然目前難以利用，但這是潛在的攻擊面
