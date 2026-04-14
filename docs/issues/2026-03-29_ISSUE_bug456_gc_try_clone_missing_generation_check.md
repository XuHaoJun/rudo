# [Bug]: Gc::try_clone 缺少 generation 檢查，與 Gc::clone 修復不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 try_inc_ref_if_nonzero 前後剛好發生 slot sweep+reuse，時間窗口極小 |
| **Severity (嚴重程度)** | Critical | 會錯誤地增加另一個物件的 ref count，導致 memory leak 或 use-after-free |
| **Reproducibility (復現難度)** | Very High | 需要精確的時序控制，單執行緒幾乎無法觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::try_clone()` (ptr.rs:1590-1639)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Gc::try_clone()` 應該與 `Gc::clone()` 有一致的 generation 檢查模式：在 `try_inc_ref_if_nonzero()` 前捕獲 `pre_generation`，在成功後驗證 generation 未變化。

### 實際行為 (Actual Behavior)
`Gc::try_clone()` 缺少 `pre_generation` 捕獲和驗證。當 slot 在 `try_inc_ref_if_nonzero()` 成功後、被 `is_allocated` 最終檢查前被 sweep 並復用時，會錯誤地返回一個指向新物件的 `Gc`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:Gc::try_clone()` (line 1590-1639):

```rust
// Line 1597-1602: 第一次 is_allocated 檢查通過
if !(*header.as_ptr()).is_allocated(idx) {
    return None;
}
// Line 1605-1610: dead/dropping/construction 檢查通過

// Line 1612: try_inc_ref_if_nonzero 成功 (ref_count > 0)
if !(*gc_box_ptr).try_inc_ref_if_nonzero() {
    return None;
}

// ❌ 缺少 pre_generation 捕獲和驗證！

// Line 1618-1624: 再次檢查 dead/dropping/construction
if (*gc_box_ptr).has_dead_flag()
    || (*gc_box_ptr).dropping_state() != 0
    || (*gc_box_ptr).is_under_construction()
{
    GcBox::undo_inc_ref(gc_box_ptr);
    return None;
}

// Line 1627-1633: 最終 is_allocated 檢查
if !(*header.as_ptr()).is_allocated(idx) {
    return None;
}
```

**TOCTOU 視角:**
1. T1 執行 `try_clone()` 通過所有前置檢查 (lines 1597-1614)
2. T2 執行 lazy sweep 並復用該 slot，generation +1
3. T1 的 `is_allocated` 檢查 (lines 1627-1633) 通過（新物件也是 allocated）
4. T1 返回 `Some(Gc)` 指向新物件，但 ref count 已被錯誤增加

**對比 `Gc::clone()` (lines 2137-2149) 有正確模式:**
```rust
let pre_generation = (*gc_box_ptr).generation();  // 捕獲
(*gc_box_ptr).inc_ref();
if pre_generation != (*gc_box_ptr).generation() {  // 驗證
    crate::ptr::GcBox::undo_inc_ref(gc_box_ptr);
    panic!("Gc::clone: slot was reused during clone (generation mismatch)");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- bug387 修復了 `Gc::clone()` 的 TOCTOU 問題，但修復未一致地應用到 `try_clone()`
- generation 機制是防止 slot reuse 的標準方式
- 這個 bug 會破壞 ref count 的正確性

**Rustacean (Soundness 觀點):**
- `is_allocated` 檢查不足以防止 TOCTOU - 需要 generation 驗證
- 錯誤的 ref count 可能導致 memory leak 或 use-after-free
- 建議: 將 generation 檢查模式標準化

**Geohot (Exploit 觀點):**
- 理論上可被利用控制 allocation timing 誘發此 race
- 極難穩定重現，但後果嚴重

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `try_inc_ref_if_nonzero()` 前新增 `pre_generation` 捕獲，在成功後驗證：

```rust
let pre_generation = (*gc_box_ptr).generation();
if !(*gc_box_ptr).try_inc_ref_if_nonzero() {
    return None;
}
if pre_generation != (*gc_box_ptr).generation() {
    GcBox::undo_inc_ref(gc_box_ptr);
    return None;
}
```
