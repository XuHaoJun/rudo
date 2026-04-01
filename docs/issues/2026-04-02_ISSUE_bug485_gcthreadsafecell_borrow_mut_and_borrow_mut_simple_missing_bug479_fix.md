# [Bug]: GcThreadSafeCell::borrow_mut and borrow_mut_simple only mark NEW pointers when incremental_active at entry

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 borrow_mut/borrow_mut_simple 和 drop 之間 incremental marking phase 發生轉換 |
| **Severity (嚴重程度)** | High | 可能導致年輕對象被錯誤回收，造成 use-after-free |
| **Reproducibility (重現難度)** | Low | 需要精確的時序控制，單線程無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut()` (`cell.rs:1062-1125`), `GcThreadSafeCell::borrow_mut_simple()` (`cell.rs:1141-1199`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 `borrow_mut()` 或 `borrow_mut_simple()` 被調用時，無論 `incremental_active` 的初始狀態如何，NEW GC 指針都應該被標記為黑色，以保持 SATB 不變性。

### 實際行為 (Actual Behavior)
`GcRwLock::write()` 已經修復 (bug479)，但 `GcThreadSafeCell::borrow_mut()` 和 `borrow_mut_simple()` 沒有修復。

這些函數僅在 `incremental_active` 為 true 時標記 NEW 指針：
- `borrow_mut()` - line 1107: `if incremental_active { mark_object_black ... }`
- `borrow_mut_simple()` - line 1184: `if incremental_active { mark_object_black ... }`

如果 `incremental_active` 從 FALSE 轉換為 TRUE 在 entry 和標記之間：
- OLD 值被記錄 (bug475 fix)
- NEW 值不被標記為黑色 (bad)

這破壞了 SATB 不變性。

---

## 🔬 根本原因分析 (Root Cause Analysis)

bug479 修復了 `GcRwLock::write()`，但相同的修復沒有應用到：
1. `GcThreadSafeCell::borrow_mut()` (cell.rs:1062-1125)
2. `GcThreadSafeCell::borrow_mut_simple()` (cell.rs:1141-1199)

對比：

**GcRwLock::write() (已修復)** - sync.rs:297-300:
```rust
// FIX bug479: Always mark GC pointers black when OLD values were recorded.
mark_gc_ptrs_immediate(&*guard, true);  // FIX bug479 fix - ALWAYS marks
```

**GcThreadSafeCell::borrow_mut_simple() (未修復)** - cell.rs:1184-1196:
```rust
// FIX bug475: Always capture old GC pointers for SATB, regardless of incremental_active.
// ...
if incremental_active {  // BUG: Uses cached incremental_active
    unsafe {
        let guard_ref = &*guard;
        let mut new_gc_ptrs = Vec::with_capacity(32);
        guard_ref.capture_gc_ptrs_into(&mut new_gc_ptrs);
        if !new_gc_ptrs.is_empty() {
            for gc_ptr in new_gc_ptrs {
                let _ = crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8);
            }
        }
    }
}
```

時序問題：
```
T1: Thread A calls borrow_mut_simple(), incremental_active = false
T2: OLD values are recorded via bug475 fix
T3: Collector starts incremental marking, incremental_active = true
T4: mark_object_black NOT called (incremental_active was false) - NEW values NOT marked!
T5: Objects only reachable from NEW values may be prematurely collected!
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要多執行緒並發測試，單執行緒無法重現。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `borrow_mut()` 和 `borrow_mut_simple()` 中的 `if incremental_active { ... }` 改為無條件標記：

1. `GcThreadSafeCell::borrow_mut()` - cell.rs:1107
2. `GcThreadSafeCell::borrow_mut_simple()` - cell.rs:1184

```rust
// 移除 if incremental_active 檢查，改為：
unsafe {
    let guard_ref = &*guard;
    let mut new_gc_ptrs = Vec::with_capacity(32);
    guard_ref.capture_gc_ptrs_into(&mut new_gc_ptrs);
    if !new_gc_ptrs.is_empty() {
        for gc_ptr in new_gc_ptrs {
            let _ = crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8);
        }
    }
}
```

並添加相同的註解：
```rust
// FIX bug485: Always mark GC pointers black when OLD values were recorded.
// If incremental becomes active between entry and here, we must mark NEW
// to maintain SATB consistency (OLD recorded, NEW must be marked too).
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 不變性要求：如果記錄了 OLD 值，相應的 NEW 值也應該被標記。如果增量標記在記錄後變為活躍，NEW 對象可能未被標記，導致它們被錯誤回收。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果 NEW 對象被錯誤回收，透過 NEW 指針訪問會導致 use-after-free。

**Geohot (Exploit 觀點):**
攻擊者可能通過控制 GC 時序來觸發此 bug，導致記憶體腐敗。

---

## 備註

- 與 bug479 相關：bug479 修復了 GcRwLock::write()，但沒有修復 GcThreadSafeCell methods
- 與 bug475 相關：bug475 修復了 borrow_mut_simple 總是記錄 OLD 值
- 需要 Miri 或 ThreadSanitizer 驗證
