# [Bug]: Lazy Sweep CAS 回滾後 Slot 遺留 allocated 標記導致記憶體洩漏

**Status:** Closed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 lazy sweep 過程中並發分配 |
| **Severity (嚴重程度)** | Medium | 每次觸發會洩漏一個 slot，累積導致記憶體浪費 |
| **Reproducibility (重現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `lazy_sweep_page` and `lazy_sweep_page_all_dead` in `gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

在 `lazy_sweep_page` 和 `lazy_sweep_page_all_dead` 函數中，當 CAS 迴圈成功將 slot 加入 free list 後，若偵測到並發配置，程式會嘗試回滾 CAS。但回滾後 `did_reclaim` 仍為 `false`，導致 `clear_allocated(i)` 不會被呼叫。Slot 因此遺留在 allocated bitmap 中，但既不在 free list 上也無法被回收，形成記憶體洩漏。

### 預期行為

當並發配置發生時，slot 應該：
1. 要嘛成功被回收（`did_reclaim = true` 且 `clear_allocated` 被呼叫）
2. 要嘛乾淨地回到 free list（回滾 CAS 成功）

### 實際行為

Slot 的 allocated 標記未被清除，且不在 free list 上，形成所謂 "orphaned slot"。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### `lazy_sweep_page` (lines 2636-2698)

```rust
let mut did_reclaim = false;  // line 2635
loop {
    // CAS 成功將 slot i 加入 free list
    Ok(_) => {
        if (*header).is_allocated(i) {  // line 2645: 並發配置檢測
            // ... 回滾 CAS ...
            all_dead = false;           // line 2678
            break;                       // line 2679: did_reclaim 仍為 false!
        }
        did_reclaim = true;  // line 2681: 未達到
        break;
    }
    // ...
}

if did_reclaim {
    (*header).clear_allocated(i);  // line 2696: 因為 did_reclaim=false，未被呼叫
    reclaimed += 1;
}
```

### `lazy_sweep_page_all_dead` (lines 2761-2822)

同樣的模式，lines 2803 `break` 時 `did_reclaim` 仍為 `false`，導致 line 2820 `clear_allocated` 未被呼叫。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要多執行緒環境，在 lazy sweep 執行期間同時觸發並發分配：

```rust
// Thread A: 執行 lazy_sweep_page 或 lazy_sweep_page_all_dead
// Thread B: 同時在該 page 上配置新物件到同一個 slot
// 驗證: slot 的 allocated 標記是否仍存在，但無法被分配使用
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在並發配置檢測分支中，回滾 CAS 後應設置 `did_reclaim = true` 以確保 `clear_allocated(i)` 被呼叫：

```rust
if (*header).is_allocated(i) {
    // ... existing rollback code ...
    did_reclaim = true;  // 添加這行，確保 clear_allocated 被呼叫
    all_dead = false;
    break;
}
```

或者在 break 之後添加：

```rust
if did_reclaim {
    (*header).clear_allocated(i);
} else {
    // 如果是並發配置導致的 break，仍需清除 allocated
    (*header).clear_allocated(i);
}
reclaimed += 1;
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 會導致 lazy sweep 在並發場景下洩漏 slot。每個 orphaned slot 都會使可用記憶體減少一個 block。長時間運行後可能導致可用的閒置 slot 耗盡，觸發不必要的頁面分配或 major GC。

**Rustacean (Soundness 觀點):**
這不是 UB，但屬於記憶體管理邏輯錯誤。Slot 雖然在 bitmap 中被標記為 allocated，但實際上不可達，可能導致內存碎片化。

**Geohot (Exploit 觀點):**
在極端情況下，累積的 orphaned slot 可能導致內存耗盡。但考慮到 lazy sweep 的執行時機（通常在 STW），實際可利用性較低。