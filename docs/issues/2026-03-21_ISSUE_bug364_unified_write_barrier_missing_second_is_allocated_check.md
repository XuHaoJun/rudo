# [Bug]: unified_write_barrier 缺少 second is_allocated check (bug364)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 mutator 並發執行，物件槽位被重用 |
| **Severity (嚴重程度)** | High | 可能導致 dirty tracking 混亂，影響 GC 正確性 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `unified_write_barrier` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在 `gc_cell_validate_and_barrier` 和 `incremental_write_barrier` 中，bug364 修復添加了 second `is_allocated` check 在 if-else block 計算 `(header, index)` **之後**、呼叫 `set_dirty` **之前**。

### 實際行為 (Actual Behavior)
`unified_write_barrier` 在 if-else block 後**缺少** second `is_allocated` check。當 slot 在通過 if-else 內的檢查後、被 `set_dirty` 前被 sweep 並重用時，會導致 dirty tracking corruption。

**gc_cell_validate_and_barrier (有 fix, lines 3007-3012):**
```rust
                (header, index)
            };

            // Skip if slot was swept; avoids corrupting dirty tracking with reused slot (bug364).
            if !(*h.as_ptr()).is_allocated(index) {
                return;
            }

            (*h.as_ptr()).set_dirty(index);
```

**unified_write_barrier (MISSING fix, lines 3094-3098):**
```rust
                    (h, index)
                };

            (*header.as_ptr()).set_dirty(index);  // <-- BUG: No second is_allocated check!
            heap.add_to_dirty_pages(header);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU race condition:
1. Thread A 在 if-else branch 中通過 `is_allocated` 檢查
2. Lazy sweep 釋放該 slot
3. 新物件分配到同一 slot
4. Thread A 執行 `set_dirty` 在已被其他物件使用的 slot 上
5. Dirty page tracking corruption

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 一個執行緒不斷分配/釋放物件重用槽位
2. 另一個執行緒不斷觸發 `unified_write_barrier`
3. 觀察 dirty_pages 是否包含無效的 slot

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `unified_write_barrier` 的 if-else block 後、呼叫 `set_dirty` 前添加 second `is_allocated` check：

```rust
                    (h, index)
                };

            // Skip if slot was swept; avoids corrupting dirty tracking with reused slot (bug364).
            if !(*header.as_ptr()).is_allocated(index) {
                return;
            }

            (*header.as_ptr()).set_dirty(index);
            heap.add_to_dirty_pages(header);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Bug364 fix 已在 `gc_cell_validate_and_barrier` 和 `incremental_write_barrier` 中實施，但 `unified_write_barrier` 被遺漏。這三個函數執行類似的操作，應該有一致的保護。

**Rustacean (Soundness 觀點):**
沒有 second check 的情況下，swept slot 可能在 if-else 和 `set_dirty` 之间被重用，導致對錯誤的 slot 設置 dirty flag。

**Geohot (Exploit 觀點):**
攻擊者可以嘗試控制 slot 重用的時序，來操縱 dirty_pages 的內容。
