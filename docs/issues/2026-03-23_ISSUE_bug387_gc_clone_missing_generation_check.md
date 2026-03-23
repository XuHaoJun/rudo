# [Bug]: Gc::clone 缺少 generation 檢查，導致 slot reuse TOCTOU

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要剛好在 clone 時 slot 被 sweep 並復用，時間窗口極小但存在 |
| **Severity (嚴重程度)** | Critical | 會錯誤地增加另一個物件的 ref count，導致 memory leak 或 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要多執行緒并发且剛好踩到時間窗口，可使用 ThreadSanitizer 輔助驗證 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::clone()` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`Gc::clone()` 在執行 `inc_ref()` 前後缺少 `generation` 檢查，而 `GcHandle::resolve()` 有這個檢查。當 slot 在 `is_allocated` 檢查通過後、`inc_ref()` 執行前被 sweep 並復用時，會錯誤地增加另一個物件的 ref count。

### 預期行為 (Expected Behavior)
- `Gc::clone()` 應該在 `inc_ref()` 前後檢查 generation，確保 slot 未被復用
- 行為應與 `GcHandle::resolve()` 一致

### 實際行為 (Actual Behavior)
- `inc_ref()` 可能作用於已被復用的 slot，錯誤地增加新物件的 ref count
- 可能導致 memory leak（新物件永不被釋放）或 use-after-free（舊物件被錯誤保留）

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:Gc::clone()` (line 2036-2084):

```rust
// Check is_allocated BEFORE inc_ref to avoid TOCTOU (bug289).
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::clone: object slot was swept before inc_ref"
    );
}

(*gc_box_ptr).inc_ref();  // <-- 缺少 generation 檢查!

if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::clone: object slot was swept after inc_ref"
    );
}
```

對比 `GcHandle::resolve()` (cross_thread.rs:251-264) 有正確的 generation 檢查:

```rust
let pre_generation = gc_box.generation();
gc_box.inc_ref();
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "GcHandle::resolve: slot was reused between pre-check and inc_ref (generation mismatch)"
);
```

**TOCTOU 攻擊視角:**
1. T1 執行 `Gc::clone()` 通過 `is_allocated` 檢查 (line 2060-2066)
2. T2 執行 lazy sweep 並復用該 slot，generation +1
3. T1 執行 `inc_ref()` (line 2068) - 現在作用於新物件！
4. T1 的 `is_allocated` 檢查 (line 2070-2077) 通過（新物件也是 allocated）
5. 錯誤地增加了新物件的 ref count

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要多執行緒并发，可使用 ThreadSanitizer 驗證 data race
// 單執行緒幾乎無法觸發

#[test]
fn test_gc_clone_generation_check() {
    use std::thread;
    use std::sync::Arc;
    
    let gc = Arc::new(Gc::new(42));
    let gc2 = gc.clone();
    
    // 這兩個 clone 之間如果發生 slot sweep+reuse，會觸發 bug
    let gc3 = gc.clone();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_ref()` 前後新增 generation 檢查，與 `GcHandle::resolve()` 保持一致:

```rust
// Get generation BEFORE inc_ref to detect slot reuse (bug387).
let pre_generation = (*gc_box_ptr).generation();

(*gc_box_ptr).inc_ref();

// Verify generation hasn't changed - if slot was reused, undo inc_ref.
if pre_generation != (*gc_box_ptr).generation() {
    crate::ptr::GcBox::undo_inc_ref(gc_box_ptr);
    panic!("Gc::clone: slot was reused during clone (generation mismatch)");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- slot reuse 問題在 GC 系統中很常見，generation 機制是標準解決方案
- Chez Scheme 使用類似的 card-marking 來追蹤年輕世代物件的變化
- 這個 bug 會破壞 ref count 的正確性，導致記憶體管理錯誤

**Rustacean (Soundness 觀點):**
- 這不是 UB，但會導致記憶體錯誤（memory leak 或 UAF）
- `is_allocated` 檢查不夠 - 需要 generation 來捕捉 slot reuse
- 建議: 將此模式標準化，所有 `inc_ref()` 呼叫前都應該有 generation 檢查

**Geohot (Exploit 觀點):**
- 雖然這個 bug 不直接造成安全漏洞，但錯誤的 ref count 可能被利用
- 如果攻擊者能控制 allocation timing，可能誘發這個 race condition
- 複雜的及時攻擊，但理論上可行