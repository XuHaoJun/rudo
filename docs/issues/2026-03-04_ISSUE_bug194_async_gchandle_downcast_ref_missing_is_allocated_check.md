# [Bug]: AsyncGcHandle::downcast_ref 缺少 is_allocated 檢查導致 UAF

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要物件被 sweep 後仍被引用，且呼叫 downcast_ref |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free (UAF) |
| **Reproducibility (復現難度)** | Medium | 需要觸發 lazy sweep 且物件被引用 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncGcHandle::downcast_ref`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`AsyncGcHandle::downcast_ref` 在解引用 `gc_box_ptr` 之前沒有檢查 `is_allocated`。如果物件已被 lazy sweep 回收並重新分配，這會導致 UAF。

### 預期行為 (Expected Behavior)
在解引用前應檢查 `is_allocated`，若物件已被回收則返回 `None`。

### 實際行為 (Actual Behavior)
直接解引用未檢查 `is_allocated` 的指標，可能讀取已釋放或重新使用的記憶體。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/handles/async.rs` 的 `downcast_ref` 函數中：

```rust
let gc_box_ptr = tcb.with_scope_lock_if_active(self.scope_id, || unsafe {
    let slot = &*self.slot;
    slot.as_ptr() as *const GcBox<T>
})?;

unsafe {
    let gc_box = &*gc_box_ptr;  // <-- 沒有 is_allocated 檢查!
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag()
        || gc_box.dropping_state() != 0
    {
        return None;
    }
    Some(gc_box.value())
}
```

相比之下，`GcHandle::resolve` (bug 193) 也有類似的問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要構建以下情境：
1. 建立 `GcScope` 並追蹤 GC 物件
2. 觸發 lazy sweep 回收該物件
3. 在不同位置重新分配同一 slot
4. 呼叫 `downcast_ref` 讀取舊指標

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在解引用 `gc_box_ptr` 前加入 `is_allocated` 檢查：

```rust
unsafe {
    let gc_box = &*gc_box_ptr;
    
    // 添加 is_allocated 檢查
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    let index = /* 計算物件索引 */;
    if !(*header.as_ptr()).is_allocated(index) {
        return None;
    }
    
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag()
        || gc_box.dropping_state() != 0
    {
        return None;
    }
    Some(gc_box.value())
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在 GC 期間回收未標記的物件，但指標可能仍保留在 handle slot 中。此時若呼叫 `downcast_ref`，會解引用已釋放的記憶體。

**Rustacean (Soundness 觀點):**
這是經典的 UAF 問題。在解引用前必須檢查物件是否仍為有效配置。

**Geohot (Exploit 觀點):**
若攻擊者能控制重新分配的內容，可能利用此漏洞進行記憶體佈局操縱。
