# [Bug]: GcHandle::resolve_impl generation mismatch panic does not undo inc_ref (ref_count leak)

**Status:** Fixed
**Tags:** Fixed

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Very Low` | 需要 slot reuse + generation overflow + precise timing |
| **Severity (嚴重程度)** | `Medium` | Ref_count leak leads to memory leak |
| **Reproducibility (重現難度)** | `Very High` | Generation overflow extremely unlikely (~4 billion allocations) |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve_impl()` (crates/rudo-gc/src/handles/cross_thread.rs:264-274)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 generation 檢測到改變時，應該使用 `undo_inc_ref` 撤銷 increment，類似於 `Handle::to_gc()`、`Handle::get()` 和 `AsyncHandle::to_gc()` 的處理方式。

### 實際行為 (Actual Behavior)
`resolve_impl()` 在 generation 改變時使用 `assert_eq!` panic，但沒有撤銷 `inc_ref()` 的 increment，導致 ref_count 洩漏。

### 程式碼位置

`crates/rudo-gc/src/handles/cross_thread.rs` 第 264-274 行：
```rust
let pre_generation = gc_box.generation();

gc_box.inc_ref();  // Increment happens here

// Verify generation hasn't changed - if slot was reused, this will panic.
// This prevents inc_ref from operating on the wrong object's ref count.
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "GcHandle::resolve: slot was reused between pre-check and inc_ref (generation mismatch)"
);  // <-- BUG: Panic 但沒有 undo_inc_ref!
```

### 對比：Handle::to_gc 的正確實現（已修復 bug455）

`crates/rudo-gc/src/handles/mod.rs` 第 416-418 行：
```rust
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("Handle::to_gc: slot was reused between pre-check and inc_ref (generation mismatch)");
}
```

### 對比：AsyncHandle::to_gc 的正確實現

`crates/rudo-gc/src/handles/async.rs` 第 860-864 行：
```rust
// FIX bug453: If generation changed, undo the increment to prevent ref_count leak.
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::to_gc: slot was reused between pre-check and inc_ref (generation mismatch)");
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題流程：
1. 線程 A 調用 `resolve_impl()`
2. 第 264 行：捕獲 `pre_generation`
3. **此時**：Slot 被 sweep 並重用，新物件 allocation 生成新的 generation
4. 第 266 行：`inc_ref()` 在新物件上成功（因為 ref_count > 0）
5. 第 270-274 行：`assert_eq` 失敗，因為 generation 已改變，**panic**
6. **但是**：increment 沒有被撤銷，導致新物件的 ref_count 錯誤 +1

這會導致：
- 新物件的 ref_count 永遠不會回到正確值
- 該物件永遠不會被 GC 回收（memory leak）

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// 理論 PoC - 需要精確控制 generation overflow 和 slot sweep 時序
// 實際上極難重現，因為需要約 40 億次 allocation 才會 overflow u32

#[test]
fn repro_bug461_resolve_impl_gen_mismatch_ref_count_leak() {
    // 這個測試在實際情況下幾乎不可能穩定重現
    // 因為需要：
    // 1. generation overflow (u32::MAX -> 0)
    // 2. Slot 在精確時間被 sweep 並重用
    // 3. resolve_impl() 在這個視窗內被調用
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `crates/rudo-gc/src/handles/cross_thread.rs` 的 `resolve_impl()`：

```rust
let pre_generation = gc_box.generation();

gc_box.inc_ref();

// FIX bug461: 如果 generation 改變，撤銷 increment 並 panic
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(self.ptr.as_ptr());
    panic!("GcHandle::resolve: slot was reused between pre-check and inc_ref (generation mismatch)");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generation 機制旨在防止 slot 重用問題。`Handle::to_gc()` (bug455)、`Handle::get()` (bug454)、和 `AsyncHandle::to_gc()` (bug453) 都已修復，但 `resolve_impl()` 漏掉了這個修復。這雖然極不可能，但確實是一個需要修復的 memory leak。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB，但導致 memory leak。如果在 panic 前有其他代碼依賴 ref_count 的正確性，可能導致邏輯錯誤。

**Geohot (Exploit 攻擊觀點):**
理論上可以通過控制分配模式強制 generation overflow，導致 memory leak。但實際利用難度極高，需要約 40 億次 allocation。

---

## 相關 issue

- bug455: Handle::to_gc() 相同問題 - 已修復
- bug454: Handle::get() 相同問題 - 已修復
- bug453: AsyncHandle::to_gc() 相同問題 - 已修復
- bug347: GcHandle::resolve_impl is_allocated check insufficient - 導致添加 generation 檢查，但未包含 undo_inc_ref