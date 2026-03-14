# [Bug]: Weak::drop Missing is_allocated Check After is_gc_box_pointer_valid

**Status:** Fixed
**Tags:** Verified

---

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要精確控制 GC timing，在 is_gc_box_pointer_valid 檢查後、weak count 操作前發生 lazy sweep slot 重用 |
| **Severity (嚴重程度)** | High | 可能導致在已釋放/重用的 slot 上執行 weak count 操作，導致錯誤的記憶體操作 |
| **Reproducibility (重現難度)** | Medium | 需要精確控制 GC timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>::drop` (ptr.rs:2180-2236)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`Weak<T>::drop` 函數在dereference GcBox 指標之前，沒有檢查該 slot 是否仍然 allocated。這與 Bug 231 (WeakCrossThreadHandle::drop) 類似的問題模式。

### 預期行為 (Expected Behavior)
`Weak<T>::drop` 應該在dereference `ptr` 之後、執行 weak count 操作之前檢查該 slot 是否仍然 allocated，確保不會訪問已釋放的記憶體。

### 實際行為 (Actual Behavior)
`Weak<T>::drop` 呼叫 `is_gc_box_pointer_valid` 後直接dereference指標並檢查：
- `has_dead_flag()`
- `dropping_state()`
- `is_under_construction()`

但缺少 `is_allocated` 檢查。如果 slot 在 `is_gc_box_pointer_valid` 檢查後、weak count 操作前被 sweep 後重用，可能會訪問新物件的 GcBox header。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:2180-2236`：

```rust
impl<T: Trace> Drop for Weak<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Relaxed);
        let Some(ptr) = ptr.as_option() else {
            return;
        };

        let ptr_addr = ptr.as_ptr() as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {  // 檢查通過
            self.ptr.set_null();
            return;
        }
        // === 這裡存在 TOCTOU 窗口 ===
        // slot 可能在此時被 sweep 並重用
        unsafe {
            let gc_box = &*ptr.as_ptr();  // <-- 直接dereference，沒有再次檢查 is_allocated
            if gc_box.has_dead_flag()
                || gc_box.dropping_state() != 0
                || gc_box.is_under_construction()
            {
                return;
            }
            // ... 執行 weak count 操作
        }
    }
}
```

問題：
1. `is_gc_box_pointer_valid` 只檢查指標是否在heap範圍內和對齊，但**不檢查 slot 是否仍然 allocated**（雖然它內部會檢查，但結果可能過時）
2. 直接dereference `ptr.as_ptr()` 而沒有再次檢查 `is_allocated`
3. 檢查的是新物件的狀態（如果 slot 被重用），而不是原始物件
4. 這可能導致在已釋放的記憶體上執行 weak count 操作

對比 `GcBoxWeakRef::clone` (ptr.rs:603-611) 有正確的檢查：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*ptr.as_ptr()).dec_weak();
        return Self {
            ptr: AtomicNullable::null(),
        };
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立 Gc object 並取得 Weak reference
2. 觸發 GC，使用 lazy sweep 回收該 object
3. 新 object 在同一個 slot 被分配
4. 在 Weak 被 drop 時，在 is_gc_box_pointer_valid 檢查後、weak count 操作前觸發另一個 GC
5. 預期：正確處理已釋放的 slot
6. 實際：可能訪問錯誤的 GcBox header

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak<T>::drop` 中新增 `is_allocated` 檢查：

```rust
impl<T: Trace> Drop for Weak<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Relaxed);
        let Some(ptr) = ptr.as_option() else {
            return;
        };

        let ptr_addr = ptr.as_ptr() as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {
            self.ptr.set_null();
            return;
        }
        
        // 新增：檢查 slot 是否仍然 allocated
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if !(*header.as_ptr()).is_allocated(idx) {
                self.ptr.set_null();
                return;
            }
        }
        
        unsafe {
            let gc_box = &*ptr.as_ptr();
            if gc_box.has_dead_flag()
                || gc_box.dropping_state() != 0
                || gc_box.is_under_construction()
            {
                return;
            }
            // ... 執行 weak count 操作
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 lazy sweep 實現中，slot 可能被回收並立即重用。如果在 drop 時沒有檢查 is_allocated，可能會讀取到新物件的 header 資訊，導致錯誤的 weak reference 計數操作。這與 WeakCrossThreadHandle::drop 的問題完全相同。

**Rustacean (Soundness 觀點):**
這可能導致 use-after-free 類型的問題。當 slot 被重用後，舊的 GcBox header 已經無效，讀取可能會得到垃圾數據，導致非預期的記憶體損壞。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能透過控制 GC timing 來觸發這個問題，進一步利用記憶體佈局進行攻擊。特別是在 is_gc_box_pointer_valid 檢查通過後、weak count 執行前的時間窗口。

---

## Resolution (2026-03-14)

**Outcome:** Fixed.

Added `is_allocated` check in `Weak<T>::drop` (ptr.rs), matching the pattern from bug231 (`WeakCrossThreadHandle::drop`). The check runs after `is_gc_box_pointer_valid` and before dereferencing the GcBox, returning early if the slot has been swept and reused. All Weak-related tests pass.
