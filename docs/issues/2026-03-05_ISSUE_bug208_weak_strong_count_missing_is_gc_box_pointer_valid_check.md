# [Bug]: Weak::strong_count() 與 Weak::weak_count() 缺少 is_gc_box_pointer_valid 檢查

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Weak::strong_count/weak_count 並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | High | 會讀取到已回收 slot 新物件的計數，導致計數錯誤 |
| **Reproducibility (重現難度)** | High | 需要精確的時序控制來觸發 slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::strong_count()`, `Weak::weak_count()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Weak::strong_count()` 與 `Weak::weak_count()` 在解引用指標之前沒有檢查 `is_gc_box_pointer_valid()`。當 slot 被 lazy sweep 回收並重新分配給新物件時，舊的 Weak 指標會讀取到新物件的 ref_count，導致計數錯誤。

此問題與 bug197, bug207 為同一模式（其他 Gc/Weak 方法缺少 is_allocated 驗證），但應用於 `Weak::strong_count()` 與 `Weak::weak_count()`。

### 預期行為 (Expected Behavior)

在解引用前應檢查 `is_gc_box_pointer_valid()`，若 slot 已被回收並重新分配則應返回 0。

### 實際行為 (Actual Behavior)

`Weak::strong_count()` (ptr.rs:2002-2023) 與 `Weak::weak_count()` (ptr.rs:2030-2051) 僅檢查：
- alignment (`ptr_addr % alignment != 0`)
- `is_under_construction()`
- `has_dead_flag()`
- `dropping_state()`

但缺少 `is_gc_box_pointer_valid()` 檢查。

對比其他 `Weak` 方法：
- `Weak::clone()` - line 2091 有 `is_gc_box_pointer_valid` 檢查
- `Weak::drop()` - 有檢查
- `Weak::try_upgrade()` - 有檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `ptr.rs:2002-2023` (`Weak::strong_count()`) 與 `ptr.rs:2030-2051` (`Weak::weak_count()`)

當 lazy sweep 回收 slot 並重新分配給新物件時：
1. 舊的 Weak 指標仍然指向同一記憶體位址
2. `Weak::strong_count()` 讀取該位址的 ref_count
3. 由於缺少 `is_gc_box_pointer_valid()` 檢查，會讀取到新物件的 ref_count（而非返回 0）
4. 這會導致呼叫者獲得錯誤的計數資訊

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制：
1. 建立 GC 物件並取得 Weak 參考
2. 觸發 lazy sweep 回收該 slot
3. 在 sweep 後立即從另一個執行緒呼叫 `Weak::strong_count()`
4. 驗證計數是否返回 0（預期）還是錯誤的值（實際）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::strong_count()` 與 `Weak::weak_count()` 中新增 `is_gc_box_pointer_valid()` 檢查，類似於 `Weak::clone()`:

```rust
// 在 Weak::strong_count() 中，新增:
let ptr_addr = ptr.as_ptr() as usize;
if !is_gc_box_pointer_valid(ptr_addr) {
    return 0;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會導致 slot 被回收並可能重新分配。當 ref_count 檢查跳過指標有效性驗證時，會讀取到新物件的計數而非舊物件的計數。這會破壞 weak ref count 的正確性。

**Rustacean (Soundness 觀點):**
缺少 `is_gc_box_pointer_valid()` 檢查會導致在 slot reuse 後讀取無效記憶體。雖然 ref_count 是 usize 類型不會直接導致 UAF，但會讀取到錯誤物件的內部狀態。

**Geohot (Exploit 觀點):**
攻擊者可能利用錯誤的 ref_count 資訊來進行計數攻擊或判斷物件生命週期。若新物件包含敏感資料，Weak 持有者可能透過計數讀取推斷記憶體佈局。

---

## Resolution (2026-03-14)

**Invalid — Already Fixed.** Investigation of `ptr.rs` shows both `Weak::strong_count()` (lines 2224–2226) and `Weak::weak_count()` (lines 2255–2256) already include the `is_gc_box_pointer_valid(ptr_addr)` check and return 0 when the pointer is invalid. The described code path no longer exists; the fix was applied in a prior commit.
