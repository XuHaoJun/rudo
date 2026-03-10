# [Bug]: AsyncHandle::to_gc() Missing is_allocated Check After Reference Count Increment

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 inc_ref 後、GcBox 被 sweep 之間的精確時序 |
| **Severity (嚴重程度)** | High | 可能導致 use-after-free 或記憶體不安全 |
| **Reproducibility (復現難度)** | High | 需要精確的執行時序，很難穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::to_gc()` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

在調用 `try_inc_ref_if_nonzero()` 增加引用計數後，應該檢查物件是否仍然有效（未被 sweep 回收）。這是其他類似程式碼中的標準模式，例如 `cross_thread.rs` 中的 `GcHandle::resolve()`。

### 實際行為 (Actual Behavior)

`AsyncHandle::to_gc()` 方法在調用 `try_inc_ref_if_nonzero()` 後，**沒有**檢查 `is_allocated`。這與其他類似程式碼不一致，可能導致在增量標記或 lazy sweep 期間出現 TOCTOU 漏洞。

### 程式碼位置

**handles/async.rs:719-729**:
```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::to_gc: object is being dropped by another thread");
}
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::to_gc: object became dead/dropping after ref increment");
}
// 缺少 is_allocated 檢查！
Gc::from_raw(gc_box_ptr as *const u8)
```

### 對比 cross_thread.rs 中的正確實現

**cross_thread.rs:210-216**:
```rust
gc_box.inc_ref();

if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        crate::ptr::GcBox::dec_ref(self.ptr.as_ptr());
        panic!("GcHandle::resolve: object slot was swept after inc_ref");
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

這是一個經典的 TOCTOU (Time-Of-Check-Time-Of-Use) 漏洞：

1. **Thread A** 調用 `AsyncHandle::to_gc()`
2. 調用 `try_inc_ref_if_nonzero()` - 成功增加引用計數
3. **Thread B** (GC thread) 執行 lazy sweep，回收同一個 slot
4. **Thread A** 繼續執行，返回 `Gc::from_raw(gc_box_ptr as *const u8)` - **此時指標指向已回收的記憶體！**

雖然有 `has_dead_flag()`、`dropping_state()` 和 `is_under_construction()` 檢查，但這些檢查並不能防止 slot 被 sweep 後重新分配的情況。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確的執行時序
// 1. 創建 AsyncHandleScope 和 AsyncHandle
// 2. 在另一個執行緒觸發 GC (特別是 lazy sweep)
// 3. 調用 to_gc()
// 4. 觀察是否發生 use-after-free
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `try_inc_ref_if_nonzero()` 調用後添加 `is_allocated` 檢查：

```rust
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::to_gc: object is being dropped by another thread");
}
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::to_gc: object became dead/dropping after ref increment");
}
// 添加 is_allocated 檢查
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(gc_box_ptr.cast_mut());
        panic!("AsyncHandle::to_gc: object slot was swept after inc_ref");
    }
}
Gc::from_raw(gc_box_ptr as *const u8)
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 系統中，引用計數增加後，必須驗證物件仍然有效。增量標記和 lazy sweep 可能會在任意時刻回收記憶體。缺少這個檢查會導致懸浮指標。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。返回一個指向已回收記憶體的 Gc 指標是未定義行為。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以嘗試在 inc_ref 和檢查之間觸發 GC，導致 use-after-free。
