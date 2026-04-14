# [Bug]: GcHandle::try_resolve_impl uses dec_ref instead of undo_inc_ref causing ref_count leak

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Very Low | Requires generation overflow + slot sweep + precise timing |
| **Severity (嚴重程度)** | Medium | ref_count leak leads to memory leak |
| **Reproducibility (復現難度)** | Very High | Generation overflow extremely unlikely (~4 billion allocations) |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::try_resolve_impl()` (handles/cross_thread.rs:411-414, 427-430)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當檢測到 slot 被重用或 sweep 時，應該使用 `undo_inc_ref` 撤銷 increment，與 `resolve_impl`、`Gc::try_clone`、`Handle::get` 等函數保持一致。

### 實際行為 (Actual Behavior)
`try_resolve_impl()` 在 generation 檢查失敗或 slot 被 sweep 時調用 `dec_ref()`，但 `dec_ref()` 當 `DEAD_FLAG` 設置時會提前返回而不遞減，導致 ref_count 洩漏。

### 程式碼位置

**handles/cross_thread.rs:411-414**:
```rust
if pre_generation != gc_box.generation() {
    GcBox::dec_ref(self.ptr.as_ptr());  // BUG: 應該用 undo_inc_ref
    return None;
}
```

**handles/cross_thread.rs:427-430**:
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    GcBox::dec_ref(self.ptr.as_ptr());  // BUG: 應該用 undo_inc_ref
    return None;
}
```

### 對比：resolve_impl 的正確實現

**handles/cross_thread.rs:271-274** (resolve_impl - 正確):
```rust
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(self.ptr.as_ptr());  // 正確：使用 undo_inc_ref
    panic!("GcHandle::resolve: slot was reused...");
}
```

**ptr.rs:1616-1619** (Gc::try_clone - 正確):
```rust
if pre_generation != (*gc_box_ptr).generation() {
    GcBox::undo_inc_ref(gc_box_ptr);  // 正確：使用 undo_inc_ref
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### dec_ref vs undo_inc_ref 的差異

根據 `ptr.rs` 的文檔 (lines 219-236):

```rust
/// Undo a `ref_count` increment after detecting that the object is dead or dropping.
///
/// Use this instead of `dec_ref` when rolling back a successful
/// `try_inc_ref_from_zero` or `try_inc_ref_if_nonzero` CAS: `dec_ref` returns early
/// without decrementing when `DEAD_FLAG` is set, leaving `ref_count` incorrectly at 1.
```

**問題流程**:
1. Thread A 調用 `try_resolve_impl()`
2. 第 407 行：呼叫 `inc_ref()` - ref_count 增加
3. **此時**：Slot 被 sweep 並被新物件重用，新物件的 `DEAD_FLAG` 可能已設置
4. 第 411 行：Generation 檢查失敗（slot 被重用）
5. 第 412 行：呼叫 `dec_ref()`
   - 如果新物件的 `DEAD_FLAG` 已設置，`dec_ref()` 提前返回不遞減
   - 但 `inc_ref()` 確實增加了 ref_count！
   - **結果**：ref_count 永遠無法回到正確值 - 記憶體洩漏

### 為什麼 line 417-422 用 dec_ref 是正確的

```rust
if gc_box.dropping_state() != 0
    || gc_box.has_dead_flag()
    || gc_box.is_under_construction()
{
    GcBox::dec_ref(self.ptr.as_ptr());
    return None;
}
```

這是對同一物件的 TOCTOU 檢查：如果在 `inc_ref()` 之後，物件變得無效（另一個執行緒正在 drop），調用 `dec_ref()` 是正確的。因為 `inc_ref()` 和 `dec_ref()` 都作用於同一物件。

但 lines 412 和 428 的情況不同：
- `inc_ref()` 在 slot 被重用之前作用於舊物件
- `dec_ref()` 在 slot 被重用之後作用於新物件
- 如果新物件有 `DEAD_FLAG`，`dec_ref()` 不遞減

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// 理論 PoC - 需要精確控制 timing
// 實際上極難重現，因為需要約 40 億次 allocation 才會 overflow u32

#[test]
fn repro_bug474_try_resolve_ref_count_leak() {
    // 這個測試在實際情況下幾乎不可能穩定重現
    // 因為需要：
    // 1. Generation overflow (u32::MAX -> 0)
    // 2. Slot 在精確時間被 sweep 並重用
    // 3. 新物件恰好有 DEAD_FLAG 設置
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

修改 `handles/cross_thread.rs` 的 `try_resolve_impl()`：

```rust
// Line 412: 將 dec_ref 改為 undo_inc_ref
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(self.ptr.as_ptr());  // 正確：總是遞減
    return None;
}

// Line 428: 將 dec_ref 改為 undo_inc_ref
if !(*header.as_ptr()).is_allocated(idx) {
    GcBox::undo_inc_ref(self.ptr.as_ptr());  // 正確：總是遞減
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generation 機制旨在防止 slot 重用問題。但如果在 generation overflow 發生時（u32::MAX + 1 = 0），理論上新舊物件 generation 可能相同。但即使不 overflow，如果 slot 被 sweep 並立即被新物件重用，generation 也會不同。問題是當新物件處於某種無效狀態時，`dec_ref()` 的 early return 導致洩漏。

**Rustacean (Soundness 觀點):**
這導致 memory leak，不是嚴格的 UB。但如果洩漏的 ref_count 累積，可能導致物件永遠不被回收。

**Geohot (Exploit 攻擊觀點):**
理論上可以通過控制分配模式強制 generation overflow 和 slot 重用，導致 memory leak。但實際利用難度極高。
