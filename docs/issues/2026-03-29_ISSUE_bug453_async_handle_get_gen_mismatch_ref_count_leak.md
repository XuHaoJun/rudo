# [Bug]: AsyncHandle::get generation mismatch panic does not undo ref_count increment (ref_count leak)

**Status:** Fixed
**Tags:** Verified

## 驗證記錄

**驗證日期:** 2026-03-29
**驗證人員:** opencode

### 驗證結果

代碼確認修復已套用 (`handles/async.rs:638-643`)：
```rust
// FIX bug453: If generation changed, undo the increment to prevent ref_count leak.
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: slot was reused before value read (generation mismatch)");
}
```

**Status: Fixed** - 修復已套用於程式碼。

## Resolution (2026-04-06)

**Outcome:** Already fixed in code.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Very Low | 需要 generation overflow + slot sweep + precise timing |
| **Severity (嚴重程度)** | Medium | ref_count leak leads to memory leak |
| **Reproducibility (復現難度)** | Very High | Generation overflow extremely unlikely (4 billion allocations) |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()` (handles/async.rs:634-642)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 generation 檢查失敗時，應該使用 `undo_inc_ref` 撤銷 increment，類似於 `GcBoxWeakRef::upgrade()` 的處理方式。

### 實際行為 (Actual Behavior)
`AsyncHandle::get()` 在 generation 改變時 panic，但沒有撤銷 `try_inc_ref_if_nonzero` 的 increment，導致 ref_count 洩漏。

### 程式碼位置

`handles/async.rs` 第 634-642 行：
```rust
let pre_generation = gc_box.generation();
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::get: object is being dropped");
}
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "AsyncHandle::get: slot was reused before value read (generation mismatch)"
);  // <-- BUG: Panic 但沒有 undo_inc_ref!
```

### 對比：GcBoxWeakRef::upgrade 的正確實現

`ptr.rs` 第 752-760 行（已修復 bug413）：
```rust
let pre_generation = gc_box.generation();
if !gc_box.try_inc_ref_if_nonzero() {
    return None;
}
// Verify generation hasn't changed - if slot was reused, undo inc_ref (bug413).
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(ptr.as_ptr());  // <-- 正確：撤銷 increment
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題流程：
1. 線程 A 調用 `AsyncHandle::get()`
2. 第 634 行：捕獲 `pre_generation`
3. **此時**：Slot 被 sweep 並重用，新物件 allocation 生成新的 generation
4. 第 635 行：`try_inc_ref_if_nonzero()` 在新物件上成功（因為 ref_count > 0）
5. 第 638-642 行：`assert_eq` 失敗，因為 generation 已改變，**panic**
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
fn repro_bug453_async_handle_get_gen_mismatch_ref_count_leak() {
    // 這個測試在實際情況下幾乎不可能穩定重現
    // 因為需要：
    // 1. generation overflow (u32::MAX -> 0)
    // 2. Slot 在精確時間被 sweep 並重用
    // 3. 新物件的 generation 恰好等於舊物件的舊 generation
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `handles/async.rs` 的 `AsyncHandle::get()`：

```rust
let pre_generation = gc_box.generation();
if !gc_box.try_inc_ref_if_nonzero() {
    panic!("AsyncHandle::get: object is being dropped");
}
// FIX bug453: 如果 generation 改變，撤銷 increment 並返回 None
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: slot was reused before value read (generation mismatch)");
}
```

或者將 assert_eq 改為使用 undo_inc_ref：

```rust
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: slot was reused before value read (generation mismatch)");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generation 機制旨在防止 slot 重用問題。但如果在 generation overflow 發生時（u32::MAX + 1 = 0），理論上可能發生新舊物件 generation 相同的情况。這雖然極不可能，但確實是一個需要修復的 memory leak。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB，但導致 memory leak。如果在 panic 前有其他代碼依賴 ref_count 的正確性，可能導致邏輯錯誤。

**Geohot (Exploit 攻擊觀點):**
理論上可以通過控制分配模式強制 generation overflow，導致 memory leak。但實際利用難度極高，需要約 40 億次 allocation。

---

## 驗證記錄

**驗證日期:** 2026-03-29
**驗證人員:** opencode

### 驗證結果

通過代碼比對確認差異：
1. `AsyncHandle::get`: 第 638-642 行使用 `assert_eq`，panic 時不撤銷 increment
2. `GcBoxWeakRef::upgrade`: 第 757-760 行使用 `undo_inc_ref`，正確處理

**Status: Open** - 需要修復。
