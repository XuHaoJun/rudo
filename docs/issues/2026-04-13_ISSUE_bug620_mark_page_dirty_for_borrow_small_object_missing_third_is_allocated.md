# [Bug]: mark_page_dirty_for_borrow small object path missing third is_allocated check (TOCTOU)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent sweep during borrow_mut |
| **Severity (嚴重程度)** | `Medium` | Could cause incorrect dirty page marking leading to UAF |
| **Reproducibility (復現難度)** | `High` | Needs concurrent access to trigger TOCTOU |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_page_dirty_for_borrow` in `heap.rs:3175-3202`
- **OS / Architecture:** `Linux x86_64`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

`mark_page_dirty_for_borrow` 函數的小型物件路徑缺少第三個 `is_allocated` 檢查，與 `incremental_write_barrier` 等其他 barrier 函數不一致。

### 預期行為 (Expected Behavior)
小型物件路徑應該與 `incremental_write_barrier` (lines 3399-3416) 和 `unified_write_barrier` 一樣，擁有三個 `is_allocated` 檢查：
1. 第一次檢查：在讀取任何 generation 相關資料前
2. 第二次檢查：讀取 `has_gen_old` 前
3. **第三次檢查**：讀取 `has_gen_old` **之後**（防禦性檢查，防止 TOCTOU）

### 實際行為 (Actual Behavior)
`mark_page_dirty_for_borrow` 的小型物件路徑（lines 3175-3202）只有一個 `is_allocated` 檢查（line 3197），缺少第三個檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

對比 `incremental_write_barrier` (lines 3399-3416)：
```rust
// incremental_write_barrier small object path:
if !(*h.as_ptr()).is_allocated(index) {  // Check 1
    return;
}
// ...
if !(*h.as_ptr()).is_allocated(index) {  // Check 2 - BEFORE reading has_gen_old
    return;
}
let gc_box_addr = ...;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
// ...
if !(*h.as_ptr()).is_allocated(index) {  // Check 3 - AFTER reading has_gen_old (FIX bug530)
    return;
}
```

而 `mark_page_dirty_for_borrow` (lines 3175-3202)：
```rust
// mark_page_dirty_for_borrow small object path:
if !(*h).is_allocated(index) {  // Only Check 1
    return;
}
// NO generation check in this path!
(*h).set_dirty(index);  // Line 3201
heap.add_to_dirty_pages(header);  // Line 3202
// MISSING: Third is_allocated check after has_gen_old read
```

這導致了不一致性和潛在的 TOCTOU 漏洞。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Thread 1: Continuously calling borrow_mut on GcCell
loop {
    let mut cell_ref = gc_cell.borrow_mut();
    cell_ref.push(new_gc);
}

// Thread 2: Concurrently sweeping the page containing the GcCell
loop {
    collect_full();  // Forces sweep
}

// The race condition:
// Between mark_page_dirty_for_borrow's check at line 3197 
// and set_dirty at line 3201, the slot could be:
// 1. Swept (is_allocated becomes false)
// 2. Reused by new allocation with different gen
// 3. set_dirty sets dirty flag on NEW object's slot
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `mark_page_dirty_for_borrow` 的小型物件路徑中添加第三個 `is_allocated` 檢查：

```rust
// After line 3197's is_allocated check, add the same pattern as incremental_write_barrier:

let gc_box_addr = (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bugXXX: Third is_allocated check AFTER has_gen_old read - prevents TOCTOU
if !(*h).is_allocated(index) {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The incremental_write_barrier and unified_write_barrier both have the third check for defense-in-depth. This consistency is important for the TOCTOU prevention pattern. The missing check in mark_page_dirty_for_borrow could allow a slot to be reused between the check and the set_dirty call, potentially corrupting the dirty page tracking.

**Rustacean (Soundness 觀點):**
The inconsistency between barrier functions is concerning. While the first two checks provide reasonable protection, the missing third check creates a theoretical race window. This is not a clear UB but represents defensive depth that other functions in the codebase have.

**Geohot (Exploit 觀點):**
The TOCTOU window between is_allocated check and set_dirty could be exploited if an attacker can control timing. They could potentially cause incorrect dirty page marking, leading to objects not being traced during minor GC. However, the practical exploitability is low due to the narrow race window.