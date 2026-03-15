# [Bug]: sweep_orphan_pages 的 has_survivors 檢查缺少 is_allocated 驗證

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 orphan pages 與 lazy sweep 交互作用 |
| **Severity (嚴重程度)** | Medium | 可能導致錯誤保留 orphan pages 或不正確的 survivor 檢測 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序條件來觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sweep_orphan_pages`, `heap.rs:3123-3128`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為

在檢查 object 是否為 survivor（透過 `is_marked`）之前，應該先檢查該 slot 是否仍然被分配（`is_allocated`），以確保不會錯誤地將已釋放並重用的 slot 視為標記狀態。

### 實際行為

`sweep_orphan_pages` 函數中的 `has_survivors` 檢查直接調用 `is_marked(i)` 而沒有先檢查 `is_allocated(i)`：

```rust
// heap.rs:3123-3128
let has_survivors = if is_large {
    (*header).is_marked(0)
} else {
    let obj_count = (*header).obj_count as usize;
    (0..obj_count).any(|i| (*header).is_marked(i))  // 缺少 is_allocated 檢查!
};
```

對比同函數中 `has_weak_refs` 的正確實作（lines 3137-3150）：

```rust
(0..obj_count).any(|i| {
    if (*header).is_allocated(i) {  // 正確：先檢查 is_allocated
        // ... check weak_count
    } else {
        false
    }
})
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

與 bug78 和 bug123 相同的模式：

1. 當 lazy sweep 運行時，orphan page 中的 slots 可能被釋放並重新分配
2. 新分配的物件可能會有 is_marked 標誌（因為 slot 被重用）
3. 沒有 is_allocated 檢查會導致 `has_survivors` 錯誤返回 true
4. 這會導致 orphan page 被錯誤保留，無法回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 啟用 lazy sweep feature
2. 分配多個 objects 並觸發 GC
3. 讓 objects 成為 orphan（透過 cross-thread handle 或其他机制）
4. 同時進行 lazy sweep 來重用 orphan page 中的 slots
5. 呼叫 `sweep_orphan_pages` 
6. 觀察 has_survivors 返回錯誤的值

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `has_survivors` 檢查中添加 `is_allocated` 驗證：

```rust
let has_survivors = if is_large {
    // Large objects: 檢查 is_allocated first
    (*header).is_allocated(0) && (*header).is_marked(0)
} else {
    let obj_count = (*header).obj_count as usize;
    (0..obj_count).any(|i| {
        if (*header).is_allocated(i) {
            (*header).is_marked(i)
        } else {
            false
        }
    })
};
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這與 bug78 和 bug123 相同的模式 - 在檢查 is_marked 之前需要先驗證 is_allocated。Orphan pages 的管理應該與 regular pages 保持一致的行為。

**Rustacean (Soundness 觀點):**
這可能導致記憶體洩漏（orphan page 錯誤保留）或不正確的 survivor 檢測。但不會導致 UAF，因為檢查的是標記狀態而非直接記憶體訪問。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個來：
1. 防止 orphan pages 被正確回收（記憶體洩漏）
2. 影響 GC 的記憶體回收效率

---

## Resolution (2026-03-13)

Added `is_allocated` check to `has_survivors` in `sweep_orphan_pages()` for both large and small object paths, matching the pattern in `has_weak_refs` and similar fixes in bug78/bug123. Full test suite passes.

---

## 備註

類似於：
- bug78: parallel marking 缺少 is_allocated 檢查
- bug123: incremental marking mark_root_for_snapshot 缺少 is_allocated 檢查