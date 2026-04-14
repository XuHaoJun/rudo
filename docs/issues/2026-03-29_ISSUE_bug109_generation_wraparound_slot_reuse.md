# [Bug]: Generation Wraparound 導致 Slot Reuse 檢測失效

**Status:** Invalid
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Very Low | 需要 u32 generation counter wraparound (2^32 次同 slot 分配) |
| **Severity (嚴重程度)** | Medium | 可能導致 tracing 錯誤物件，但有 generation 差異檢測作為第二道防線 |
| **Reproducibility (復現難度)** | Very Low | 理論上可重現，但實際上幾乎不可能 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `scan_page_for_marked_refs`, `scan_page_for_unmarked_refs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

Generation-based slot reuse 檢測依賴於 `marked_generation != current_generation` 來判斷 slot 是否被回收並重新分配。然而，當 `AtomicU32` generation counter 發生 wraparound (從 `u32::MAX` 回到 `0`) 時，相同的 generation 值可能出現於不同的物件，導致錯誤的物件被 tracing。

### 預期行為
- Slot 被 sweep + reallocation 後，generation 應該不同
- Generation 差异應被檢測並 skip 該物件

### 實際行為
- 當 generation wraparound 發生時，sweep + reallocation 後的 generation 可能與標記時相同
- 導致 `current_generation == marked_generation` 通過檢查，但實際上 slot 已被重用

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc/incremental.rs` 的 `scan_page_for_marked_refs` (lines 818-849) 和 `scan_page_for_unmarked_refs` (lines 964-985) 中：

```rust
// Line 824/969: Capture generation after successful mark
let marked_generation = unsafe { (*gc_box_ptr).generation() };

// Line 828/974: Check is_allocated
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}

// Line 835/981: Verify generation hasn't changed
let current_generation = unsafe { (*gc_box_ptr).generation() };
if current_generation != marked_generation {
    break; // Slot was reused - skip
}
```

Generation 機制假設：
1. Allocation 時 generation 遞增
2. Sweep 時 generation 不變
3. Reallocation 時 generation 再遞增

但當 `u32` generation counter wraparound 時：
- `generation=X` 的 slot 被標記
- Slot 被 sweep (generation 仍為 X)
- Slot 被 reallocation 多次，最終 generation 再次變為 X (wraparound)
- `current_generation == marked_generation`，檢測失效

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論 PoC（極難穩定重現）：

```rust
// 需要非常多次的同 slot 分配來觸發 wraparound
#[test]
fn test_generation_wraparound_traces_wrong_object() {
    // 1. Allocate object A in slot with generation 0
    // 2. Force generation to MAX via many allocations
    // 3. Mark slot (captures generation MAX)
    // 4. Force sweep + many allocations to wrap generation to 0
    // 5. Allocate new object B with generation 0
    // 6. Now current_generation == marked_generation (0 == 0)
    // 7. Object B would be incorrectly traced
    
    // Expected: Object B should be traced as live (since mark was from A)
    // Actual: Object B IS traced (this is actually correct per SATB!)
}
```

註：此場景是 SATB (Snapshot-At-The-Beginning)  semantics 的預期行為。問題是generation wraparound 可能導致標記轉移到不同物件。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

考慮使用額外的 "slot state" 版本號來追蹤 slot 重用，與 generation 分開：

```rust
// In GcBox, track slot reuse separately from generation
struct GcBox<T> {
    generation: AtomicU32,
    slot_version: AtomicU32,  // 新增：每次 sweep/realloc 遞增
}

// 檢測時不只檢查 generation，也要檢查 slot_version
if slot_version != marked_slot_version {
    break; // Slot was reused
}
```

或者，增加 generation 的大小至 `u64` 以降低 wraparound 機率（但仍是理論上可能的）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Generation wraparound 在實際 GC 中極為罕見（需要 2^32 次同 slot 分配）
- Chez Scheme 使用類似的 generation 機制，但通過大counter 和快速的 turnover 避免此問題
- 影響：`marked_generation == current_generation` 可能導致 tracing 錯誤物件，但概率極低
- 建議：可視為 low priority，現有的世代檢測已提供基本保護

**Rustacean (Soundness 觀點):**
- 這不是嚴格的 soundness bug，因為generation 差異檢測失敗不等於 memory safety violation
- 潛在風險：如果錯誤物件被 tracing，可能讀取無關資料
- 建議：添加 slot_version 或使用 u64 generation 來彻底消除此理論風險

**Geohot (Exploit 觀點):**
- 理論上可利用：攻擊者可通過大量分配嘗試觸發 wraparound
- 實際難度：需要 2^32 次分配來保證觸發，實際不可行
- 建議：專注於更實際的攻擊面

---

## Resolution (2026-04-12)

**Outcome:** Invalid - impractical to reproduce.

**Reasoning:**
1. **Practical impossibility**: Triggering generation wraparound requires ~4 billion (2^32) allocations to the SAME slot. At modern allocation rates, this would take years of continuous allocation to one slot.
2. **SATB semantics**: The issue itself acknowledges (line 89) that "Object B IS traced (this is actually correct per SATB!)" - this is expected behavior, not a bug.
3. **Never verified**: The issue is marked "Unverified" and was documented as a theoretical concern without actual reproduction.
4. **Risk assessment**: Even if triggered, the risk is low - it could cause incorrect tracing but not memory safety violations.

**Conclusion:** This is a theoretical concern that was documented for completeness but is practically impossible to reproduce. The generation mechanism provides adequate protection for real-world scenarios.
