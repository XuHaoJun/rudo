# [Bug]: Weak::strong_count() 與 Weak::weak_count() 缺少 is_allocated 檢查導致讀取錯誤計數

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Weak::strong_count/weak_count 並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | Medium | 返回錯誤的引用計數，可能導致應用程式邏輯錯誤 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::strong_count()`, `Weak::weak_count()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`Weak::strong_count()` 與 `Weak::weak_count()` 在解引用 `gc_box_ptr` 之前沒有檢查 `is_allocated`。如果 slot 被 lazy sweep 回收並重新分配給新物件，舊的 Weak 指標會讀取到新物件的 ref_count/weak_count，導致返回錯誤的計數值。

### 預期行為 (Expected Behavior)
在解引用前應檢查 `is_allocated`，若物件已被回收應返回 0 或適當的錯誤值。

### 實際行為 (Actual Behavior)
直接解引用指標，讀取已釋放或重新使用的記憶體中的計數值。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs` 的 `Weak::strong_count()` (lines 2090-2114) 與 `Weak::weak_count()` (lines 2121-2145) 中：

1. 載入指標並檢查對齊與有效性
2. 檢查 `is_gc_box_pointer_valid()`
3. 檢查 `is_under_construction()`, `has_dead_flag()`, `dropping_state()`

但缺少 `is_allocated()` 檢查。當 slot 被 lazy sweep 回收並重新分配時，舊指標可能指向新物件。

對比 `GcHandle::resolve()` (handles/cross_thread.rs:210-216) 正確實現了此檢查：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        // handle swept slot
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立 `Gc<T>` 物件並取得 `Weak<T>`
2. 觸發 lazy sweep 回收該物件
3. 在同一 slot 重新分配新物件
4. 呼叫 `Weak::strong_count()` 或 `Weak::weak_count()`
5. 觀察返回值為新物件的計數而非 0

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::strong_count()` 與 `Weak::weak_count()` 中新增 `is_allocated` 檢查：

```rust
// 在 Weak::strong_count() 中，檢查 is_allocated
unsafe {
    let gc_box = &*ptr.as_ptr();
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag()
        || gc_box.dropping_state() != 0
    {
        0
    } else {
        // 新增 is_allocated 檢查
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
            let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
            if let Some(header) = header {
                if !(*header.as_ptr()).is_allocated(idx) {
                    return 0; // slot 已回收
                }
            }
        }
        gc_box.ref_count().get()
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在 GC 期間回收未標記的物件，但 Weak 指標可能仍保留在 application code 中。此時若呼叫 `Weak::strong_count()` 等方法，會讀取已釋放或重新使用的記憶體中的計數值。這與 bug197 (Gc 核心方法缺少 is_allocated 檢查) 為同一模式。

**Rustacean (Soundness 觀點):**
這是記憶體安全的問題。雖然不像 UAF 那樣嚴重，但讀取錯誤的計數值可能導致應用程式邏輯錯誤。

**Geohot (Exploit 觀點):**
若攻擊者能控制重新分配的內容，可能利用此漏洞進行記憶體佈局操縱，但難度較高。

---

## 🔗 相關 Issue

- bug197: Gc 核心方法缺少 is_allocated 檢查
- bug208: Weak::strong_count 缺少 is_gc_box_pointer_valid 檢查
- bug261: Weak::strong_count/weak_count 缺少 MIN_VALID_HEAP_ADDRESS 檢查

---

## Resolution (2026-03-15)

**Outcome:** Fixed.

Added `is_allocated` check to both `Weak::strong_count()` and `Weak::weak_count()` in `ptr.rs` before dereferencing the GcBox. When `ptr_to_object_index` returns `Some(idx)` and `!is_allocated(idx)`, the methods now return 0 instead of reading potentially reused slot memory. Matches the pattern used in `Weak::upgrade`, `Weak::clone`, and `GcHandle::resolve`.
