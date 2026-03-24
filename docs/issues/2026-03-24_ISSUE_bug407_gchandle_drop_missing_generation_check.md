# [Bug]: GcHandle::drop 缺少 generation 檢查可能導致 ref_count 損壞

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需在 remove 與 dec_ref 之間發生 lazy sweep slot 重用 |
| **Severity (嚴重程度)** | Critical | 可能導致錯誤的物件被 drop，造成 use-after-free |
| **Reproducibility (復現難度)** | Very High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::drop` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::drop` 應該在调用 `dec_ref` 之前檢查 generation，確保 slot 沒有被 sweep 和重用。

### 實際行為 (Actual Behavior)

`GcHandle::drop` 在調用 `dec_ref` 之前只檢查 `is_allocated`，但沒有檢查 generation。如果在 `roots.strong.remove()` 和 `dec_ref()`行之間發生 lazy sweep 並且 slot 被重新分配，則 `dec_ref` 會作用於新物件的 ref_count，導致錯誤的物件被 drop。

### 程式碼位置

`handles/cross_thread.rs` 第 700-727 行：

```rust
impl<T: Trace + 'static> Drop for GcHandle<T> {
    fn drop(&mut self) {
        if self.handle_id == HandleId::INVALID {
            return;
        }
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.remove(&self.handle_id);  // <- 此時 slot 仍有效
            drop(roots);
        } else {
            let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
        }
        self.handle_id = HandleId::INVALID;

        unsafe {
            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    return;  // <- 只檢查 is_allocated
                }
            }
        }
        // BUG: 沒有 generation 檢查！
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());  // <- 如果 slot 被重用，會作用於錯誤的物件
    }
}
```

### 對比：`resolve_impl` 有正確的 generation 檢查

`resolve_impl` 在調用 `inc_ref` 前有 generation 檢查（第 264-274 行）：

```rust
let pre_generation = gc_box.generation();
gc_box.inc_ref();
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "GcHandle::resolve: slot was reused between pre-check and inc_ref (generation mismatch)"
);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Race Window:**
1. T1 調用 `GcHandle::drop`
2. T1 從 `roots.strong` 移除 entry
3. T1 釋放 `roots` 鎖
4. [Race Window] Lazy sweep 運行，sweep 掉 slot，並重新分配給新物件
5. T1 調用 `dec_ref(self.ptr)` - 但此時 `self.ptr` 指向的是新物件！
6. 新物件的 ref_count 被錯誤遞減
7. 如果 ref_count 變成 0，新物件被錯誤 drop

**為何現有檢查不足：**
- `is_allocated` 檢查只驗證 slot 在檢查時是否已分配
- 在 `is_allocated` 檢查和 `dec_ref` 調用之間，slot 可能被 sweep 並重新分配
- 缺少 generation 檢查來確保我們正在操作正確的物件

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::drop` 中新增 generation 檢查，類似 `resolve_impl` 的做法：

```rust
impl<T: Trace + 'static> Drop for GcHandle<T> {
    fn drop(&mut self) {
        if self.handle_id == HandleId::INVALID {
            return;
        }
        
        let pre_generation = unsafe { (*self.ptr.as_ptr()).generation() };
        
        if let Some(tcb) = self.origin_tcb.upgrade() {
            let mut roots = tcb.cross_thread_roots.lock().unwrap();
            roots.strong.remove(&self.handle_id);
            drop(roots);
        } else {
            let _ = heap::remove_orphan_root(self.origin_thread, self.handle_id);
        }
        self.handle_id = HandleId::INVALID;

        unsafe {
            if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    return;
                }
            }
            
            // 新增：generation 檢查
            let current_generation = (*self.ptr.as_ptr()).generation();
            if pre_generation != current_generation {
                panic!("GcHandle::drop: slot was reused during drop (generation mismatch)");
            }
        }
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典型的 TOCTOU (Time-of-Check-Time-of-Use) bug。在 GC 系統中，slot 重用是一個常見的優化，但我們需要在操作 ref_count 前確保 slot 仍然是原始物件。generation 機制正是為此設計的。

**Rustacean (Soundness 觀點):**
這可能導致 use-after-free 和記憶體腐敗。如果錯誤的物件被 drop，其記憶體可能被後續分配重用，導致同一記憶體位置有兩個活躍的 Rust 參考。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能控制 GC 時序，可能利用此 bug 導致特定物件被提前 drop，進一步利用 drop callback 中的指標操作。
