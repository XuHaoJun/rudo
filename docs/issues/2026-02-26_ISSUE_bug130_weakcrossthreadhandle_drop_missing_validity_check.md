# [Bug]: WeakCrossThreadHandle::drop 缺少有效性檢查可能導致 UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 WeakCrossThreadHandle 在 object 被回收後才 drop |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | High | 需要精確控制時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::drop()`, `handles/cross_thread.rs:535-545`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::drop()` 應該在遞減 weak_count 之前檢查指標是否仍然有效。如果 object 已經被回收（記憶體可能被重用），則不應該調用 `dec_weak()`。

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::drop()` 直接調用 `dec_weak()` 而不檢查 object 是否仍然有效：

```rust
impl<T: Trace + 'static> Drop for WeakCrossThreadHandle<T> {
    fn drop(&mut self) {
        let ptr = self.weak.as_ptr();
        let Some(ptr) = ptr else {
            return;
        };
        unsafe {
            (*ptr.as_ptr()).dec_weak();  // <-- BUG: 沒有有效性檢查！
        }
    }
}
```

`as_ptr()` 只檢查指標是否為 null，但不檢查：
1. 指標是否在 heap 範圍內
2. GcBox 是否已經被 sweep 回收
3. 記憶體是否已被重用

### 程式碼位置

`handles/cross_thread.rs` 第 535-545 行：

```rust
impl<T: Trace + 'static> Drop for WeakCrossThreadHandle<T> {
    fn drop(&mut self) {
        let ptr = self.weak.as_ptr();
        let Some(ptr) = ptr else {
            return;
        };
        // BUG: 這裡應該先檢查指標是否仍然有效
        unsafe {
            (*ptr.as_ptr()).dec_weak();
        }
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `WeakCrossThreadHandle::drop()` 實現與 `Weak::drop()` (ptr.rs:2000-2057) 不一致。`Weak::drop()` 正確地調用 `is_gc_box_pointer_valid()` 來檢查指標有效性：

```rust
// ptr.rs 中的正確實現
impl<T: Trace> Drop for Weak<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Relaxed);
        let Some(ptr) = ptr.as_option() else {
            return;
        };

        let ptr_addr = ptr.as_ptr() as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {  // <-- 正確：先檢查有效性
            self.ptr.set_null();
            return;
        }
        // ... 安全地 dec_weak
    }
}
```

而 `WeakCrossThreadHandle::drop()` 缺少這個關鍵的有效性檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 在 thread A 創建一個 Gc 物件和其 WeakCrossThreadHandle
2. 將 WeakCrossThreadHandle 傳遞給 thread B
3. 在 thread A drop 原始 Gc 並觸發 GC
4. 在 GC sweep 後，thread B 的 WeakCrossThreadHandle 被 drop
5. 此時記憶體可能被重用，但 drop() 仍嘗試調用 dec_weak()

注意：這個場景比較難以穩定重現，因為需要精確控制 GC 時序。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在調用 `dec_weak()` 之前添加有效性檢查，類似於 `Weak::drop()` 的實現：

```rust
impl<T: Trace + 'static> Drop for WeakCrossThreadHandle<T> {
    fn drop(&mut self) {
        let ptr = self.weak.as_ptr();
        let Some(ptr) = ptr else {
            return;
        };
        
        // 修復：添加有效性檢查
        let ptr_addr = ptr.as_ptr() as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {
            return;
        }
        
        unsafe {
            (*ptr.as_ptr()).dec_weak();
        }
    }
}
```

需要導入 `is_gc_box_pointer_valid` 函數（從 ptr.rs）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個經典的 GC 記憶體管理問題。當 weak reference 指向的 object 已被回收時，不應該嘗試修改其 metadata。雖然 weak_count 是 atomic，但記憶體可能被重用，導致未定義行為。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。缺少有效性檢查可能導致 Use-After-Free，在最佳情況下會 crash，最壞情況下可能被攻擊者利用。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能夠控制 GC 時序，可能可以：
1. 讓 WeakCrossThreadHandle drop
2. 在 dec_weak() 執行時記憶體已被重用
3. 通過控制新分配的記憶體內容來實現任意記憶體寫入

---

## 備註

此問題與其他 TOCTOU 問題不同：
- bug119/120/121: TOCTOU 發生在 upgrade 過程中
- 本 bug: TOCTOU 發生在 drop 過程中，缺少基本的安全性檢查

---

## Resolution (2026-02-27)

**Outcome:** Fixed.

Added `is_gc_box_pointer_valid(ptr_addr)` check in `WeakCrossThreadHandle::drop()` before calling `dec_weak()`, matching the pattern used in `Weak::drop()` (ptr.rs). When the GcBox has been swept and memory may be reused, drop now returns early without dereferencing. Exposed `is_gc_box_pointer_valid` as `pub` in ptr.rs for use by handles/cross_thread.rs.
