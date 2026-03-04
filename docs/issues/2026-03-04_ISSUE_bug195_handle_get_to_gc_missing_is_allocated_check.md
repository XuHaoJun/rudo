# [Bug]: Handle::get / Handle::to_gc 缺少 is_allocated 檢查導致 UAF

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 handle 存取並發執行 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free (UAF) |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::get()`, `Handle::to_gc()` in `handles/mod.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`Handle::get()` 和 `Handle::to_gc()` 在解引用 `gc_box_ptr` 之前沒有檢查 `is_allocated`。如果物件已被 lazy sweep 回收並重新分配，這會導致 UAF。

### 預期行為 (Expected Behavior)
在解引用前應檢查 `is_allocated`，若物件已被回收則 panic 或返回錯誤。

### 實際行為 (Actual Behavior)
直接解引用未檢查 `is_allocated` 的指標，可能讀取已釋放或重新使用的記憶體。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/handles/mod.rs` 的 `Handle::get()` 函數中 (lines 301-313)：

```rust
pub fn get(&self) -> &T {
    unsafe {
        let slot = &*self.slot;
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        let gc_box = &*gc_box_ptr;  // <-- 沒有 is_allocated 檢查!
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "Handle::get: cannot access a dead, dropping, or under construction Gc"
        );
        gc_box.value()
    }
}
```

同樣的問題也存在於 `Handle::to_gc()` (lines 347-362)。

對比 `GcHandle::resolve` (bug193) 和 `AsyncGcHandle::downcast_ref` (bug194) 也有類似的問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要構建以下情境：
1. 建立 `HandleScope` 並追蹤 GC 物件
2. 透過 handle 取得物件
3. 觸發 lazy sweep 回收該物件
4. 在不同位置重新分配同一 slot
5. 呼叫 `handle.get()` 或 `handle.to_gc()` 讀取舊指標

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在解引用 `gc_box_ptr` 前加入 `is_allocated` 檢查：

```rust
pub fn get(&self) -> &T {
    unsafe {
        let slot = &*self.slot;
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        
        // 添加 is_allocated 檢查
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        let index = /* 計算物件索引 */;
        if let Some((header, index)) = header.zip(index) {
            assert!(
                (*header.as_ptr()).is_allocated(index),
                "Handle::get: slot has been swept and reused"
            );
        }
        
        let gc_box = &*gc_box_ptr;
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "Handle::get: cannot access a dead, dropping, or under construction Gc"
        );
        gc_box.value()
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在 GC 期間回收未標記的物件，但指標可能仍保留在 handle slot 中。此時若呼叫 `handle.get()`，會解引用已釋放的記憶體。這與 bug193 (GcHandle::resolve) 和 bug194 (AsyncGcHandle::downcast_ref) 是同樣的模式。

**Rustacean (Soundness 觀點):**
這是經典的 UAF 問題。在解引用前必須檢查物件是否仍為有效配置。

**Geohot (Exploit 觀點):**
若攻擊者能控制重新分配的內容，可能利用此漏洞進行記憶體佈局操縱。

---

## 🔗 相關 Issue

- bug193: GcHandle::resolve 缺少 is_allocated 檢查
- bug194: AsyncGcHandle::downcast_ref 缺少 is_allocated 檢查
