# [Bug]: Weak::clone 缺少 Generation 檢查 - 落後於其他類似函數

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 clone 並發執行，slot 被回收並重用 |
| **Severity (嚴重程度)** | High | 會導致 weak count 操作錯誤物件，造成記憶體泄漏 |
| **Reproducibility (復現難度)** | High | 需要精確控制執行緒調度與 GC timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>::clone` in `crates/rudo-gc/src/ptr.rs:2671-2733`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`Weak::clone` 函數在调用 `inc_weak()` 前有 `is_allocated` 檢查（bug344 的修復），但缺少 `generation` 檢查來檢測 slot 是否在 `is_allocated` 檢查後、`inc_weak` 前被 sweep 並重用。

### 預期行為 (Expected Behavior)

所有類似函數都應該使用 "pre-generation + inc + post-generation 驗證" 模式：
- `GcBoxWeakRef::clone` (lines 773-784)
- `Gc<T>::as_weak` (lines 1839-1846)
- `Gc::downgrade` (lines 1764-1774)
- `Weak::upgrade` (lines 2286-2318)

這些函數都會：
1. 在操作前讀取 `pre_generation`
2. 執行遞增操作 (`inc_weak` 或 `inc_ref`)
3. 驗證 `generation` 是否改變
4. 如果改變，undo 操作並返回

### 實際行為 (Actual Behavior)

`Weak::clone` 只有 `is_allocated` 檢查，沒有 `generation` 檢查：

```rust
// ptr.rs:2707-2728 - Weak::clone
// Check is_allocated BEFORE inc_weak to avoid TOCTOU with lazy sweep (bug344).
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return ...;
    }
}
(*ptr.as_ptr()).inc_weak();  // ← 沒有 generation 檢測!

if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return ...;  // ← 返回 null 但沒有 undo inc_weak!
    }
}
```

對比 `GcBoxWeakRef::clone` 的正確模式：

```rust
// ptr.rs:773-784 - GcBoxWeakRef::clone (CORRECT)
let pre_generation = (*ptr.as_ptr()).generation();
(*ptr.as_ptr()).inc_weak();
if pre_generation != (*ptr.as_ptr()).generation() {
    (*ptr.as_ptr()).dec_weak();  // ← 正確 undo
    return Self { ptr: AtomicNullable::null() };
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU Race Condition 詳細過程：

1. Object A 存在於 slot index，weak_count = 0
2. Thread A 呼叫 `Weak::clone()`
3. Thread A 檢查 `is_allocated(idx)` → 返回 `true`（Object A 仍存在）
4. **Race Window**: Lazy sweep 在此時運行：
   - 認定 Object A 已死亡，回收 slot
   - 在同一 slot 分配 Object B（generation++）
5. Thread A 執行 `inc_weak()` → **Object B 的 weak_count 變成 1！**
6. `is_allocated` 檢查：slot 已分配（Object B），返回 true
7. 返回新 `Weak` 指向 Object B，但 Object B 的 weak_count 錯誤

後果：
- Object B 的 weak_count 錯誤地為 1，永遠不會歸零
- 當 Object B 應該被回收時，會因為錯誤的 weak_count 而無法被回收
- Object A 的 weak_count 永遠為 0

bug344 的修復只添加了 `is_allocated` 前置檢查，但沒有添加 `generation` 檢查。`is_allocated` 檢查通過不代表 slot 沒有被 sweep 和重用！

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要並發測試環境
use rudo_gc::{Gc, Weak, Trace};
use std::thread;
use std::sync::Arc;
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 創建 Gc 並獲取 Weak
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // 2. 觸發 GC 回收 gc（但 weak 仍存在）
    drop(gc);
    collect_full();
    
    // 3. 在精確的時序窗口呼叫 weak.clone()
    // 當 slot 被 lazy sweep 回收並分配給新物件後、clone 的 is_allocated 檢查前
    
    // 4. 觀察: 新物件的 weak_count 是否正確
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::clone` 中添加 `generation` 檢查（與 `GcBoxWeakRef::clone` 一致）：

```rust
impl<T: Trace> Clone for Weak<T> {
    fn clone(&self) -> Self {
        // ... 現有檢查 ...
        
        // Get generation BEFORE inc_weak to detect slot reuse.
        let pre_generation = (*ptr.as_ptr()).generation();
        
        (*ptr.as_ptr()).inc_weak();
        
        // Verify generation hasn't changed - if slot was reused, undo inc_weak.
        if pre_generation != (*ptr.as_ptr()).generation() {
            (*ptr.as_ptr()).dec_weak();
            return Self {
                ptr: AtomicNullable::null(),
            };
        }
        
        // ... 其餘代碼 ...
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 漏洞。Generation 檢查是必要的，因為 `is_allocated` 檢查通過只能證明 slot 當前被分配，不能證明 slot 沒有被 sweep 並重用於同一個 allocation 週期內。GcBoxWeakRef::clone 已經展示了正確的模式。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。錯誤的 weak_count 會導致：
1. 物件無法被正確回收（記憶體泄漏）
2. weak reference 追蹤混亂
3. 可能導致 use-after-free 如果錯誤的物件被回收但 weak ref 仍存在

**Geohot (Exploit 攻擊觀點):**
攻擊者可能通過控制 GC timing 來觸發此 race condition，導致：
1. 記憶體泄漏（物件無法回收）
2. 破壞 weak reference 的安全性質
3. 結合其他 bugs 可能造成更嚴重的後果