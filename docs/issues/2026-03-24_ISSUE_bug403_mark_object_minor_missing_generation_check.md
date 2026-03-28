# [Bug]: mark_object_minor 缺少 generation 檢查導致可能錯誤清除標記

**Status:** Fixed
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發：lazy sweep 回收插槽 + 同一插槽期間標記對象 |
| **Severity (嚴重程度)** | Medium | 可能導致存活物件被錯誤回收（需要特定並發條件） |
| **Reproducibility (復現難度)** | Medium | 需要 Miri 或 TSan 檢測 TOCTOU 種族 |

---

## 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell` write barrier, incremental marking, `mark_object_minor`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

當 `mark_object_minor` 成功標記一個物件後，如果發現該插槽已被 lazy sweep 回收並重用，應該檢查 generation 是否改變。如果 generation 不同，表示插槽已被新物件占用，不應清除標記。

### 實際行為 (Actual Behavior)

`mark_object_minor` (gc/gc.rs:2104-2108) 在 `try_mark` 成功但 `is_allocated` 返回 false 時，直接清除標記，沒有檢查 generation 是否改變。

---

## 根本原因分析 (Root Cause Analysis)

`mark_object_minor` 的程式碼：

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(index) {
        (*header.as_ptr()).clear_mark_atomic(index);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

對比 `mark_and_trace_incremental` (gc/gc.rs:2468-2477) 的正確實作：

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != marked_generation {
            return;  // 插槽被重用，不清除標記
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

關鍵差異：`mark_and_trace_incremental` 在清除標記前檢查 generation 是否改變。

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發場景：
1. 執行增量標記期間
2. `mark_object_minor` 嘗試標記物件 A（成功設置標記位）
3. Lazy sweep 在 `try_mark` 和 `is_allocated` 檢查之間運行
4. 插槽被回收並分配新物件 B（generation 增加）
5. `is_allocated` 返回 true（因為 B 已分配）
6. `mark_object_minor` 讀取 generation，與 `marked_generation` 不同
7. 但 `mark_object_minor` 不檢查這個差異，直接清除標記（儘管有 generation 差異）

實際上，由於 `is_allocated` 在步驟 3-6 間可能返回 false（如果 B 還沒分配），場景略有不同。

真正問題：**當 `is_allocated` 返回 false 時，`mark_object_minor` 總是清除標記，沒有 generation 檢查**。

---

## 建議修復方案 (Suggested Fix / Remediation)

在 `mark_object_minor` 中新增 generation 檢查，與 `mark_and_trace_incremental` 保持一致：

```rust
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();
    if !(*header.as_ptr()).is_allocated(idx) {
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != marked_generation {
            return;  // 插槽被重用，不清除標記
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
mark_object 和 mark_object_minor 的實現不一致。mark_object 正確處理了 TOCTOU，但 mark_object_minor 缺少相同的防護。增量標記和 minor GC 都使用這些函數，需要一致處理。

**Rustacean (Soundness 觀點):**
這可能導致 UAF：如果插槽被重用且 generation 相同（不太可能但可能），我們會錯誤清除新物件的標記，導致其被回收。

**Geohot (Exploit 觀點):**
TOCTOU 漏洞可用於破壞 GC 不變性。在並發條件下操縱標記清除可能導致釋放後使用。

---

## Resolution (2026-03-28)

**Outcome:** Fixed in tree.

`mark_object_minor` in `crates/rudo-gc/src/gc/gc.rs` now captures `marked_generation` after a successful `try_mark`, and when `is_allocated` is false it compares `current_generation` to `marked_generation` and returns without clearing the mark if the slot was reused (same pattern as `mark_object` / incremental paths).

Verification: `cargo test -p rudo-gc test_mark_object_minor --lib --all-features -- --test-threads=1` passes.
