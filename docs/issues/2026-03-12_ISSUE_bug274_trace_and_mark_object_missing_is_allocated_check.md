# [Bug]: trace_and_mark_object 缺少 is_allocated 檢查可能導致 UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需 incremental marking 與 lazy sweep 並發執行 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體破壞 |
| **Reproducibility (復現難度)** | High | 需精確的時序控制觸發 race condition |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking (`trace_and_mark_object` in `gc/incremental.rs`)
- **OS / Architecture:** Linux x86_64 (All platforms)
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`trace_and_mark_object` 應該像 `mark_and_trace_incremental` (gc/gc.rs:2385-2413) 一樣，在處理指標前先驗證 `is_allocated`，確保指標指向的 slot 仍被使用。

### 實際行為
`trace_and_mark_object` 直接呼叫 `trace_fn` 而沒有任何驗證。當 lazy sweep 與 incremental marking 並發執行時，slot 可能已被 sweep 並重新分配，導致指標指向無效記憶體。

---

## 🔬 根本原因分析 (Root Cause Analysis)

`trace_and_mark_object` (incremental.rs:780-794) 缺少關鍵的安全檢查：

```rust
unsafe fn trace_and_mark_object(gc_box: NonNull<GcBox<()>>, state: &IncrementalMarkState) {
    // 缺少 magic 檢查
    // 缺少 is_allocated 檢查
    
    ((*gc_box.as_ptr()).trace_fn)(data_ptr, &mut visitor);  // 直接調用 trace_fn
    // ...
}
```

對比 `mark_and_trace_incremental` (gc/gc.rs:2385-2413) 有完整檢查：
```rust
if (*header.as_ptr()).magic != crate::heap::MAGIC_GC_PAGE {
    return;
}
// ...
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
```

此函數從 `mark_slice` (line 692) 呼叫，透過 `state.pop_work()` 取得指標。若 lazy sweep 在 marking 期間並發執行並reuse了 slot，會造成 UAF。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 incremental marking feature
2. 啟動多執行緒 GC 工作
3. 在 marking 進行時觸發 lazy sweep
4. 觀察可能的記憶體錯誤

```rust
// 需要並發 timing 控制，難以穩定重現
```

**Note:** 依據 Pattern 1 (full GC 遮蔽 barrier bug)，此 bug 可能在 minor GC + incremental marking 情境下更明顯。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `trace_and_mark_object` 中新增與 `mark_and_trace_incremental` 相同的檢查：

1. 檢查 page magic (`MAGIC_GC_PAGE`)
2. 檢查 slot 是否已分配 (`is_allocated`)

若檢查失敗則 early return。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Incremental marking 的核心假設是標記期間物件不會消失（SATB）。缺少 `is_allocated` 檢查破壞了這個假設，導致標記完整性受損。當 lazy sweep 與標記並發執行時，slot 重用會導致標記錯誤的物件。

**Rustacean (Soundness 觀點):**
此函數在 unsafe 區塊中直接解引用指標，沒有任何驗證。這是經典的 TOCTOU (Time-of-check to time-of-use) 漏洞，可能導致 undefined behavior。

**Geohot (Exploit 觀點):**
若攻擊者能控制 timing，可能利用此漏洞進行 use-after-free 攻擊。透過精確控制 lazy sweep 時機，可以讓 GC 追蹤已釋放的記憶體，進一步利用記憶體佈局進行 exploit。

---

## Resolution (2026-03-15)

Added magic and `is_allocated` checks to `trace_and_mark_object` in `gc/incremental.rs`, matching the validation in `mark_and_trace_incremental` (gc/gc.rs). Early return on invalid page magic or swept slot prevents UAF when lazy sweep runs concurrently with incremental marking.