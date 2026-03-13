# [Bug]: Weak::raw_addr() 與 Weak::ptr_eq() 缺少 is_gc_box_pointer_valid 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Weak::raw_addr/ptr_eq 並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | Medium | 會返回錯誤的記憶體位址或比較結果，導致邏輯錯誤 |
| **Reproducibility (重現難度)** | High | 需要精確的時序控制來觸發 slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::raw_addr()`, `Weak::ptr_eq()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Weak::raw_addr()` 與 `Weak::ptr_eq()` 在解引用指標之前沒有檢查 `is_gc_box_pointer_valid()`。當 slot 被 lazy sweep 回收並重新分配給新物件時，舊的 Weak 指標會讀取到新物件的記憶體位址，導致返回錯誤的位址或比較結果。

此問題與 bug208 為同一模式（其他 Weak 方法缺少 is_allocated 驗證），但應用於 `Weak::raw_addr()` 與 `Weak::ptr_eq()`。

### 預期行為 (Expected Behavior)

在解引用前應檢查 `is_gc_box_pointer_valid()`，若 slot 已被回收並重新分配則應返回安全的預設值（raw_addr 返回 0，ptr_eq 返回 false）。

### 實際行為 (Actual Behavior)

`Weak::raw_addr()` (ptr.rs:2067-2072) 僅返回載入的指標位址，無有效性驗證。
`Weak::ptr_eq()` (ptr.rs:2061-2062) 僅比較指標位址，無有效性驗證。

對比其他 Weak 方法：
- `Weak::clone()` - 有 `is_gc_box_pointer_valid` 檢查
- `Weak::strong_count()` / `Weak::weak_count()` - 缺少檢查（bug208）
- `Weak::upgrade()` - 有檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `ptr.rs:2067-2072` (`Weak::raw_addr()`) 與 `ptr.rs:2061-2062` (`Weak::ptr_eq()`)

當 lazy sweep 回收 slot 並重新分配給新物件時：
1. 舊的 Weak 指標仍然指向同一記憶體位址
2. `Weak::raw_addr()` 返回該位址，但該位址現在屬於新物件
3. `Weak::ptr_eq()` 比較時可能錯誤地返回 true（新物件恰好佔用相同位址）
4. 呼叫者基於錯誤的位址或比較結果進行操作，可能導致邏輯錯誤

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制：
1. 建立 GC 物件並取得 Weak 參考
2. 呼叫 Weak::raw_addr() 記錄原始位址
3. 觸發 lazy sweep 回收該 slot
4. 在相同記憶體位置建立新物件
5. 再次呼叫 Weak::raw_addr()，驗證是否錯誤返回新物件的位址

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::raw_addr()` 與 `Weak::ptr_eq()` 中新增 `is_gc_box_pointer_valid()` 檢查：

```rust
// Weak::raw_addr()
pub fn raw_addr(&self) -> usize {
    let ptr = self.ptr
        .load(Ordering::Acquire)
        .as_option()
        .map_or(0, |p| p.as_ptr() as usize);
    
    if ptr != 0 && !is_gc_box_pointer_valid(ptr) {
        return 0;
    }
    ptr
}

// Weak::ptr_eq()
pub fn ptr_eq(this: &Self, other: &Self) -> bool {
    let this_ptr = this.ptr.load(Ordering::Acquire);
    let other_ptr = other.ptr.load(Ordering::Acquire);
    
    // Add validity check
    if let Some(this_ptr_val) = this_ptr.as_option() {
        let this_addr = this_ptr_val.as_ptr() as usize;
        if !is_gc_box_pointer_valid(this_addr) {
            return false;
        }
    }
    if let Some(other_ptr_val) = other_ptr.as_option() {
        let other_addr = other_ptr_val.as_ptr() as usize;
        if !is_gc_box_pointer_valid(other_addr) {
            return false;
        }
    }
    
    this_ptr.as_ptr() == other_ptr.as_ptr()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會導致 slot 被回收並可能重新分配。當指標驗證跳過時，會返回錯誤的記憶體位址。這會破壞 weak reference 的正確性，導致程式基於舊物件的位址進行錯誤的邏輯判斷。

**Rustacean (Soundness 觀點):**
缺少 `is_gc_box_pointer_valid()` 檢查會導致在 slot reuse 後讀取無效記憶體或進行錯誤的指標比較。這不會直接導致 UAF，但會導致程式邏輯錯誤。

**Geohot (Exploit 觀點):**
攻擊者可能利用錯誤的 raw_addr 或 ptr_eq 結果來推斷記憶體佈局或進行計數攻擊。若新物件包含敏感資料，Weak 持有者可能透過 raw_addr 讀取新物件的位址。

---

## Resolution (2026-03-14)

**Outcome:** Fixed.

Added `is_gc_box_pointer_valid()` checks to both `Weak::raw_addr()` and `Weak::ptr_eq()` in `ptr.rs`, consistent with `Weak::strong_count()`, `Weak::weak_count()`, and `Weak::clone()`. When the slot has been swept (or pointer is invalid), `raw_addr()` now returns 0 and `ptr_eq()` returns false.
