# [Bug]: GcRwLockWriteGuard 與 GcMutexGuard Drop 實現存在 TOCTOU - capture_gc_ptrs_into 與 mark_object_black 之间存在 Race

**Status:** Fixed
**Tags:** Verified

---

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並發場景：Drop 執行時剛好有 GC sweep 運行 |
| **Severity (嚴重程度)** | Medium | 可能導致錯誤標記，但 mark_object_black 已有防護 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard::drop()` 和 `GcMutexGuard::drop()` in `sync.rs:415-427` 和 `sync.rs:662-674`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcRwLockWriteGuard` 和 `GcMutexGuard` 的 Drop 實現應該在標記 GC 指針之前驗證對象是否仍然有效，確保不會錯誤地標記已被 sweep 回收的 slot。

### 實際行為 (Actual Behavior)

Drop 實現分兩步操作：
1. 調用 `capture_gc_ptrs_into` 獲取 GC 指針列表
2. 調用 `mark_object_black` 標記每個指針

這兩步之間存在 TOCTOU window：

```rust
// sync.rs:415-427
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);  // Step 1: 獲取指針

        // TOCTOU window: 在這裡 slot 可能被 sweep 並重用
        for gc_ptr in ptrs {
            let _ = unsafe { crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8) };  // Step 2: 標記
        }
    }
}
```

雖然 `mark_object_black` 內部有 `is_allocated` 檢查，但這個 TOCTOU window 可能導致：
1. 捕獲的指針指向已被回收的 slot
2. slot 已被新對象重用
3. 標記操作可能作用於新對象的 metadata

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `capture_gc_ptrs_into` 和 `mark_object_black` 不是原子操作：

1. `capture_gc_ptrs_into` 從 guard 的內部值獲取 GC 指針
2. 在獲取和標記之間，lazy sweep 可能運行
3. 如果 slot 被 sweep 並重用，標記可能作用於新對象

雖然 `mark_object_black` 會檢查 `is_allocated`，但：
- 這依賴於內部檢查來防止問題
- 概念上，調用者應該在調用標記之前驗證有效性

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確時序控制：
1. 線程 A 持有 `GcRwLockWriteGuard` 或 `GcMutexGuard`
2. 線程 A 準備 drop guard
3. 線程 B 運行 lazy sweep，剛好回收並重用同一個 slot
4. 線程 A 的 drop 實現捕獲指針，然後標記

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在調用 `mark_object_black` 之前添加明確的有效性檢查：

```rust
impl<T: GcCapture + ?Sized> Drop for GcRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let mut ptrs = Vec::with_capacity(32);
        self.guard.capture_gc_ptrs_into(&mut ptrs);

        for gc_ptr in ptrs {
            // 明確檢查是否仍然有效
            if let Some(idx) = crate::heap::ptr_to_object_index(gc_ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(gc_ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    continue; // 跳過已回收的 slot
                }
            }
            let _ = unsafe { crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8) };
        }
    }
}
```

或者，由於 `mark_object_black` 已經有這個檢查，可以在文檔中說明這個行為。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 TOCTOU 雖然有潛在風險，但 `mark_object_black` 已經有 `is_allocated` 檢查來防止錯誤標記。概念上，這是 defensive programming 的問題 - 應該在捕獲指針後立即驗證有效性，而不是依賴下游函數。

**Rustacean (Soundness 觀點):**
雖然不會導致嚴重的內存損壞（因為有內部檢查），但這不符合 Rust 的「fail fast」原則。應該盡早發現問題，而不是繼續執行。

**Geohot (Exploit 攻擊觀點):**
利用這個 TOCTOU 比較困難，因為需要精確控制 GC 時序。但攻擊者可能嘗試利用這個窗口來影響 GC 標記行為。

---

## 備註

- 這個問題的嚴重程度較低，因為 `mark_object_black` 已經有防護
- 建議是添加明確的檢查以提高代碼清晰度
- 或者在文檔中說明這個行為是預期的

---

## Resolution (2026-03-03)

**Outcome:** Fixed via documentation.

The TOCTOU window between `capture_gc_ptrs_into` and `mark_object_black` is already handled by `mark_object_black` itself: it checks `is_allocated` before marking and uses post-CAS validation to roll back if the slot was swept between check and mark (see `gc/incremental.rs:999-1024`). Adding an explicit pre-check would introduce another TOCTOU (check-then-call) and would be redundant.

Documentation was added to both `GcRwLockWriteGuard` and `GcMutexGuard` Drop implementations explaining that `mark_object_black` handles swept slots internally, so no explicit pre-check is needed.
