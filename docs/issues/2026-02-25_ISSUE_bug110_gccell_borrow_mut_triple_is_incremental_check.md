# [Bug]: GcCell::borrow_mut 三次調用 is_incremental_marking_active 導致 TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發 incremental marking phase 改變，三次調用增加 TOCTOU 窗口 |
| **Severity (嚴重程度)** | Medium | 可能導致 write barrier 失效或不必要的 barrier |
| **Reproducibility (復現難度)** | High | 需精確時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut`, `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

在 `cell.rs:155-208` 的 `GcCell::borrow_mut` 函數中，`is_incremental_marking_active()` 被調用三次。這造成嚴重的 TOCTOU (Time-of-check to time-of-use) 競爭條件，比 `trigger_write_barrier` 的雙重調用更嚴重。

### 預期行為
`borrow_mut` 應該在檢查和調用之間保持一致的狀態，確保 SATB barrier 和 generational barrier 的正確性。

### 實際行為
`is_incremental_marking_active()` 在以下位置被調用：
1. Line 161: 檢查是否需要捕獲舊值 (SATB)
2. Line 185: 傳遞給 `gc_cell_validate_and_barrier` (內部再次調用)
3. Line 190: 檢查是否需要標記新指標 (Dijkstra barrier)

三次調用之間，phase 可能多次改變，導致：
- 部分 barrier 觸發但其他部分未觸發
- 狀態不一致導致記憶體錯誤

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/cell.rs:155-208`:

```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    // 第一次調用：SATB barrier - 捕獲舊值
    if crate::gc::incremental::is_incremental_marking_active() {  // <-- Line 161
        unsafe {
            let value = &*self.inner.as_ptr();
            let mut gc_ptrs = Vec::with_capacity(32);
            value.capture_gc_ptrs_into(&mut gc_ptrs);
            if !gc_ptrs.is_empty() {
                crate::heap::with_heap(|heap| {
                    for gc_ptr in gc_ptrs {
                        if !heap.record_satb_old_value(gc_ptr) {
                            // ...
                        }
                    }
                });
            }
        }
    }

    // 第二次調用：傳遞給 barrier 函數
    crate::heap::gc_cell_validate_and_barrier(
        ptr,
        "borrow_mut",
        crate::gc::incremental::is_incremental_marking_active(), // <-- Line 185
    );

    let result = self.inner.borrow_mut();

    // 第三次調用：Dijkstra barrier - 標記新指標為黑色
    if crate::gc::incremental::is_incremental_marking_active() {  // <-- Line 190
        unsafe {
            let new_value = &*result;
            let mut new_gc_ptrs = Vec::with_capacity(32);
            new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
            if !new_gc_ptrs.is_empty() {
                crate::heap::with_heap(|_heap| {
                    for gc_ptr in new_gc_ptrs {
                        let _ = crate::gc::incremental::mark_object_black(
                            gc_ptr.as_ptr() as *const u8
                        );
                    }
                });
            }
        }
    }

    result
}
```

問題：
1. `is_incremental_marking_active()` 讀取 `IncrementalMarkState::phase()`
2. 使用 Relaxed ordering 讀取 phase
3. 三次調用之間，phase 可能多次改變
4. 每次調用可能得到不同的結果

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確控制時序：
1. 啟動 incremental marking
2. 在 `borrow_mut` 的三次調用之間中斷
3. 改變 phase

理論上可能導致：
- 部分 barrier 未觸發（記憶體錯誤）
- 不必要的 barrier 觸發（性能損失）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

```rust
pub fn borrow_mut(&self) -> RefMut<'_, T>
where
    T: GcCapture,
{
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    // 緩存 incremental marking 狀態
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    
    // 使用一致的狀態進行所有 barrier 操作
    if incremental_active {
        // SATB barrier - 捕獲舊值
        unsafe {
            let value = &*self.inner.as_ptr();
            let mut gc_ptrs = Vec::with_capacity(32);
            value.capture_gc_ptrs_into(&mut gc_ptrs);
            if !gc_ptrs.is_empty() {
                crate::heap::with_heap(|heap| {
                    for gc_ptr in gc_ptrs {
                        if !heap.record_satb_old_value(gc_ptr) {
                            // ...
                        }
                    }
                });
            }
        }
    }

    // Generational barrier
    if generational_active || incremental_active {
        crate::heap::gc_cell_validate_and_barrier(
            ptr,
            "borrow_mut",
            incremental_active,  // 使用緩存的狀態
        );
    }

    let result = self.inner.borrow_mut();

    // Dijkstra barrier - 標記新指標為黑色
    if incremental_active {
        unsafe {
            let new_value = &*result;
            let mut new_gc_ptrs = Vec::with_capacity(32);
            new_value.capture_gc_ptrs_into(&mut new_gc_ptrs);
            if !new_gc_ptrs.is_empty() {
                crate::heap::with_heap(|_heap| {
                    for gc_ptr in new_gc_ptrs {
                        let _ = crate::gc::incremental::mark_object_black(
                            gc_ptr.as_ptr() as *const u8
                        );
                    }
                });
            }
        }
    }

    result
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 比 trigger_write_barrier 的雙重調用更嚴重，因為這裡有三個不同的 barrier 操作，每個都可能因為 phase 改變而產生不一致的行為。如果在捕獲舊值後但標記新值前 phase 改變，可能導致部分指標未被標記，導致年輕物件被錯誤回收。

**Rustacean (Soundness 觀點):**
這是並發安全問題。使用 Relaxed ordering 讀取 phase，且在多次讀取之間無同步，導致可觀察的競爭行為。

**Geohot (Exploit 觀點):**
在高負載並發環境中，攻擊者可能利用此 TOCTOU 觸發不一致的 barrier 行為，進一步利用記憶體管理漏洞。

---

## Resolution Note (2026-02-26)

**Fixed.** Cached `is_incremental_marking_active()` once at the start of `borrow_mut` and reused the value for SATB barrier, `gc_cell_validate_and_barrier`, and Dijkstra barrier. All three barrier operations now use a consistent incremental marking state, eliminating the TOCTOU window.
