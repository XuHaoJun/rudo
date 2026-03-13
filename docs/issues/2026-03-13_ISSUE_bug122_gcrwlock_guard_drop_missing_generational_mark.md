# [Bug]: GcRwLock Guard Drop 缺少generational barrier的mark_object_black調用

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當generational barrier啟用但incremental marking未啟用時發生 |
| **Severity (嚴重程度)** | High | 可能導致GC錯誤回收對象 |
| **Reproducibility (復現難度)** | Medium | 需要minor GC測試 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sync.rs` - `GcRwLockReadGuard::drop()`, `GcRwLockWriteGuard::drop()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcRwLockReadGuard` 和 `GcRwLockWriteGuard` 的 Drop 實作應該在 **either** incremental marking **or** generational barrier 啟用時標記GC指針為黑色。這與 `GcThreadSafeRefMut::drop()` 的行為一致。

### 實際行為 (Actual Behavior)

當 `generational_active` 為 true 但 `incremental_active` 為 false 時，Guard的 Drop 不會調用 `mark_object_black()`，導致 NEW→OLD 引用不被正確追蹤。

### 程式碼位置

`sync.rs` 第 393 行 (`GcRwLockReadGuard::drop`):
```rust
if incremental_active {  // <-- BUG: 缺少 || generational_active
    for gc_ptr in &ptrs {
        let _ = unsafe {
            crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
        };
    }
}
```

`sync.rs` 第 454 行 (`GcRwLockWriteGuard::drop`):
```rust
if incremental_active {  // <-- BUG: 缺少 || generational_active
    // ...
}
```

### 對比：GcThreadSafeRefMut 的正確實現

`cell.rs` 第 1287-1289 行:
```rust
// Mark new GC pointers black when either barrier is active (bug122: match GcCell behavior).
// Generational barrier also requires marking new pointers for OLD->YOUNG reference tracking.
if incremental_active || generational_active {
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `sync.rs` 的 GcRwLock guard Drop 實現中，mark_object_black 調用的條件只檢查 `incremental_active`，但沒有考慮 `generational_active`。根據 bug122 的修復說明，generational barrier 也需要標記新的 GC 指針為黑色，以確保 OLD→YOUNG 引用被正確追蹤。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要minor GC測試（使用 `collect()` 而非 `collect_full()`）:
1. 設置generational barrier active但incremental marking inactive
2. 創建包含Gc指針的GcRwLock
3. 在持有write guard時修改Gc指針
4. 調用minor GC (`collect()`)
5. 驗證young對象是否被錯誤回收

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `sync.rs` 中 both GcRwLockReadGuard 和 GcRwLockWriteGuard 的 Drop 實現:

```rust
// GcRwLockReadGuard::drop (line ~393)
if incremental_active || generational_active {
    for gc_ptr in &ptrs {
        let _ = unsafe {
            crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
        };
    }
}

// GcRwLockWriteGuard::drop (line ~454)
if incremental_active || generational_active {
    for gc_ptr in &ptrs {
        let _ = unsafe {
            crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
        };
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generational barrier 需要標記新的GC指針為黑色，因為 OLD→YOUNG 引用需要被記錄到dirty pages。如果不標記，年輕對象可能在minor GC時被錯誤回收。

**Rustacean (Soundness 觀點):**
這不是UB，但是API行為不一致。GcThreadSafeRefMut和GcRwLock應該有一致的行為。

**Geohot (Exploit 攻擊觀點):**
可能導致memory corruption如果攻擊者能控制GC時機。

---

## 修復狀態

- [ ] 已修復
- [x] 未修復