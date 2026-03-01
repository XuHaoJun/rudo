# [Bug]: GcCell::borrow_mut generational barrier state 未緩存導致潛在 TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 incremental_active 緩存後、barrier 調用前觸發 GC fallback |
| **Severity (嚴重程度)** | Medium | 可能導致 write barrier 行為不一致 |
| **Reproducibility (復現難度)** | Medium | 需要精確控制 GC 時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut()`, `trigger_write_barrier_with_incremental()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`GcCell::borrow_mut()` 應該緩存 incremental_active 狀態以避免 TOCTOU (bug116)，並在調用 write barrier 時使用該緩存值。

### 實際行為
`borrow_mut()` 正確緩存了 `incremental_active` 狀態，但 `trigger_write_barrier_with_incremental()` 內部又調用 `is_generational_barrier_active()` 獲取最新狀態。這導致：

1. `incremental_active` 在調用點被緩存
2. `generational_active` 在 barrier 內部即時獲取
3. 兩者獲取的時間點不同，可能導致 barrier 使用過時的 incremental_active 值

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 代碼位置
`cell.rs` 第 1046 行和第 1137-1143 行：

```rust
// cell.rs:1046 - 緩存 incremental_active
let incremental_active = crate::gc::incremental::is_incremental_marking_active();

// ... 中間可能有 GC 狀態變化 ...

// cell.rs:1137-1143 - 調用 barrier
fn trigger_write_barrier_with_incremental(&self, incremental_active: bool) {
    let ptr = std::ptr::from_ref(self).cast::<u8>();

    // 這裡又獲取一次狀態，與 cached 的 incremental_active 時間點不同！
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    if generational_active || incremental_active {
        crate::heap::unified_write_barrier(ptr, incremental_active);
    }
}
```

### 問題分析
1. `borrow_mut()` 在第 1046 行緩存 `incremental_active`
2. 中間可能發生很多操作（包括可能的 GC fallback 請求）
3. `trigger_write_barrier_with_incremental()` 在第 1140 行調用 `is_generational_barrier_active()`
4. 最後調用 `unified_write_barrier(ptr, incremental_active)` 使用的是緩存的值

這導致 `incremental_active` 和 `generational_active` 的獲取時間點不一致。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確控制時序的 PoC
// 1. 調用 borrow_mut()，緩存 incremental_active = true
// 2. 在 barrier 調用前，觸發 GC fallback（使 generational_active = false）
// 3. 觀察 barrier 使用的是緩存的 incremental_active 值
```

**注意：** 此問題實際影響可能較小，因為 barrier 內部會做正確的檢查。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：同樣緩存 generational_active
```rust
pub fn borrow_mut(&self) -> GcThreadSafeRefMut<'_, T>
where
    T: Trace + GcCapture,
{
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();

    // ... SATB recording ...

    self.trigger_write_barrier_cached(incremental_active, generational_active);
}

fn trigger_write_barrier_cached(&self, incremental_active: bool, generational_active: bool) {
    if generational_active || incremental_active {
        crate::heap::unified_write_barrier(ptr, incremental_active);
    }
}
```

選項 2：移除參數，直接在 barrier 內部獲取
```rust
fn trigger_write_barrier_with_incremental(&self) {
    let incremental_active = crate::gc::incremental::is_incremental_marking_active();
    let generational_active = crate::gc::incremental::is_generational_barrier_active();
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在增量式 GC 中，狀態的一致性很重要。雖然 barrier 內部會做正確的檢查，但緩存狀態的時間點應該盡量接近 barrier 執行的時間點。

**Rustacean (Soundness 觀點):**
這不是嚴重的 soundness 問題，因為 barrier 內部會做正確的檢查。但可能導致性能問題（多餘的 barrier 觸發）或不一致的行為。

**Geohot (Exploit 攻擊觀點):**
很難利用這個問題，因為需要精確控制 GC 時序。但理論上可以觸發多餘的 barrier 導致輕微的性能損失。

---

## 關聯 Issue

- bug116: GcCell::borrow_mut TOCTOU (已修復 incremental_active 緩存)
- bug133: unified_write_barrier missing gen_old_flag cache

---

## Resolution (2026-03-02)

**Outcome:** Fixed.

Applied Option 1 from the suggested fix: cache `generational_active` alongside `incremental_active` in `GcThreadSafeCell::borrow_mut()` at the same point, and pass both to `trigger_write_barrier_with_incremental()`. Updated the function signature to accept `generational_active` as a second parameter. `trigger_write_barrier()` (used by `borrow_mut_simple`) now fetches both values fresh and passes both. Behavior matches `GcCell::borrow_mut()` (line 164–165).
