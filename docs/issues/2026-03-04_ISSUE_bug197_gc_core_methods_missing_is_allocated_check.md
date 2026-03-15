# [Bug]: Gc 核心方法缺少 is_allocated 檢查導致潛在 UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Gc 存取並發執行 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free (UAF) |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::as_ptr()`, `Gc::internal_ptr()`, `Gc::ref_count()`, `Gc::weak_count()`, `Gc::try_clone()`, `Gc::downgrade()`, `Weak::upgrade()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

多個 `Gc` 和 `Weak` 的核心方法在解引用 `gc_box_ptr` 之前沒有檢查 `is_allocated`。如果物件已被 lazy sweep 回收並重新分配，這會導致 UAF。

此問題與 bug195 (Handle::get/Handle::to_gc) 和 bug196 (AsyncHandle::get) 為同一模式，但影響範圍擴展到核心 `Gc` 類型本身。

### 受影響的函數 (位於 `crates/rudo-gc/src/ptr.rs`)

1. **`Gc::as_ptr()`** (line 1280) - 獲取內部指標
2. **`Gc::internal_ptr()`** (line 1296) - 獲取內部指標
3. **`Gc::ref_count()`** (line 1346) - 獲取引用計數
4. **`Gc::weak_count()`** (line 1369) - 獲取弱引用計數
5. **`Gc::try_clone()`** (line 1240) - 嘗試克隆
6. **`Gc::downgrade()`** (line 1406) - 降級為 Weak
7. **`Weak::upgrade()`** (line 1782) - 升級為 Gc

### 預期行為 (Expected Behavior)
在解引用前應檢查 `is_allocated`，若物件已被回收則返回錯誤或 panic。

### 實際行為 (Actual Behavior)
直接解引用未檢查 `is_allocated` 的指標，可能讀取已釋放或重新使用的記憶體。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/ptr.rs` 的多個函數中，訪問 `gc_box` 前只檢查了：
- `has_dead_flag()`
- `dropping_state()`
- `is_under_construction()`

但缺少 `is_allocated()` 檢查。當 slot 被 lazy sweep 回收並重新分配時，舊指標可能指向新物件，導致 UAF。

以 `Gc::as_ptr()` 為例 (lines 1280-1293)：
```rust
pub fn as_ptr(&self) -> *const T {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::as_ptr: cannot get ptr of a dead Gc");
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::as_ptr: cannot get ptr of a dead, dropping, or under construction Gc"
        );
        std::ptr::addr_of!((*gc_box_ptr).value)  // <-- 沒有 is_allocated 檢查!
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要構建以下情境：
1. 建立 `Gc` 物件
2. 透過各種方法取得指標
3. 觸發 lazy sweep 回收該物件
4. 在不同位置重新分配同一 slot
5. 呼叫受影響的方法讀取舊指標

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在每個受影響的函數中，解引用 `gc_box_ptr` 前加入 `is_allocated` 檢查：

```rust
pub fn as_ptr(&self) -> *const T {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::as_ptr: cannot get ptr of a dead Gc");
    let gc_box_ptr = ptr.as_ptr();
    
    // 添加 is_allocated 檢查
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if let Some(header) = header {
        let index = /* 計算物件索引 */;
        assert!(
            (*header.as_ptr()).is_allocated(index),
            "Gc::as_ptr: slot has been swept and reused"
        );
    }
    
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::as_ptr: cannot get ptr of a dead, dropping, or under construction Gc"
        );
        std::ptr::addr_of!((*gc_box_ptr).value)
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在 GC 期間回收未標記的物件，但指標可能仍保留在 application code 中。此時若呼叫 `Gc::as_ptr()` 等方法，會解引用已釋放的記憶體。這與 bug195 (Handle::get) 和 bug196 (AsyncHandle::get) 是同樣的模式，只是影響範圍擴展到核心 Gc 類型。

**Rustacean (Soundness 觀點):**
這是經典的 UAF 問題。在解引用前必須檢查物件是否仍為有效配置。

**Geohot (Exploit 觀點):**
若攻擊者能控制重新分配的內容，可能利用此漏洞進行記憶體佈局操縱。

---

## 🔗 相關 Issue

- bug195: Handle::get / Handle::to_gc 缺少 is_allocated 檢查
- bug196: AsyncHandle::get 缺少 is_allocated 檢查

---

## Resolution (2026-03-14)

**Outcome:** Fixed.

Added `is_allocated` check before dereferencing `gc_box_ptr` in all affected methods:

- **Gc::as_ptr**, **Gc::internal_ptr**, **Gc::ref_count**, **Gc::weak_count**: Pre-dereference assert using `ptr_to_object_index` + `ptr_to_page_header` + `is_allocated(idx)`.
- **Gc::try_clone**: Pre-dereference check returning `None` when slot swept.
- **Gc::downgrade**: Pre-dereference assert (in addition to existing post-inc_weak check).
- **Weak::upgrade**, **Weak::try_upgrade**: Pre-dereference check returning `None` when slot swept.

Pattern matches Handle::get, Handle::to_gc, and AsyncHandle::get. Large objects (ptr_to_object_index returns None) skip the check, consistent with existing code.
