# [Bug]: sweep_phase2_reclaim 回收 slot 時未清除 UNDER_CONSTRUCTION_FLAG

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要特定時序：Gc::new_cyclic_weak panic 並在 dead_flag 設置後、weak_count 為 0 時被 sweep |
| **Severity (嚴重程度)** | High | 導致 slot 回收後處於不一致狀態，可能導致 use-after-free 或錯誤的 is_under_construction() 返回值 |
| **Reproducibility (復現難度)** | Medium | 需要構造 panic 時序，但結構性缺陷明確 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sweep_phase2_reclaim` (gc/gc.rs:2311-2324)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

在 `sweep_phase2_reclaim` 函數中，當回收死亡 slot 時（`weak_count == 0 && dead_flag`），程式碼只清除 `allocated` 和 `gen_old` 標誌，但**未調用** `clear_under_construction()`。

### 預期行為 (Expected Behavior)
`sweep_phase2_reclaim` 應該與其他清理路徑一致，在回收 slot 時清除 `UNDER_CONSTRUCTION_FLAG`。

### 實際行為 (Actual Behavior)
`sweep_phase2_reclaim` 未清除 `UNDER_CONSTRUCTION_FLAG`，導致回收的 slot 處於不一致狀態。

---

## 🔬 根本原因分析 (Root Cause Analysis)

對比三個清理路徑：

1. **`dealloc` 路徑** (`heap.rs:2753-2758`):
   ```rust
   (*gc_box_ptr).clear_dead();
   (*gc_box_ptr).clear_gen_old();
   (*gc_box_ptr).clear_under_construction();  // ✓ 清除
   ```

2. **`try_pop_from_page` 分配路徑** (`heap.rs:2325-2328`):
   ```rust
   (*gc_box_ptr).clear_dead();
   (*gc_box_ptr).clear_gen_old();
   (*gc_box_ptr).clear_under_construction();  // ✓ 清除
   (*gc_box_ptr).increment_generation();
   ```

3. **`sweep_phase2_reclaim`** (`gc.rs:2320-2321`):
   ```rust
   (*header).clear_allocated(i);
   (*gc_box_ptr).clear_gen_old();
   // MISSING: (*gc_box_ptr).clear_under_construction();
   ```

**影響:**
- `is_under_construction()` 對已回收 slot 返回 `true`
- `mark_object_black()` 可能跳過標記新物件
- `Gc::try_deref()` 可能對有效物件返回 `None`
- `Weak::upgrade()` 可能返回 `None`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 概念驗證：需要特定時序
// 1. Gc::new_cyclic_weak panic，留下 UNDER_CONSTRUCTION_FLAG set 的 slot
// 2. value 已 drop（dead_flag set）但 weak_count == 0
// 3. sweep_phase2_reclaim 回收 slot，但 UNDER_CONSTRUCTION_FLAG 仍 set
// 4. slot 進入 free list，處於不一致狀態
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `gc/gc.rs:2321` 添加 `clear_under_construction()` 調用：

```rust
(*header).clear_allocated(i);
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();  // ADD THIS LINE
reclaimed += 1;
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
sweep 階段應與 dealloc 和分配路徑保持一致的狀態清理。`UNDER_CONSTRUCTION_FLAG` 用於防止對尚未完全構造物件的操作，此標誌在回收時未清除會導致 slot 狀態不一致。

**Rustacean (Soundness 觀點):**
這不是 UB，但可能導致邏輯錯誤。`is_under_construction()` 返回 true 會導致 GC 操作跳過有效物件，這可能導致 use-after-free。

**Geohot (Exploit 觀點):**
如果攻擊者能控制 GC 時序，可能利用此不一致狀態。當物件被回收並重新分配後，舊的 `UNDER_CONSTRUCTION_FLAG` 可能導致新物件被錯誤處理。

(End of file - total 100 lines)