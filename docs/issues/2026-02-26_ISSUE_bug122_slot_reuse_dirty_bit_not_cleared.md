# [Bug]: Slot Reuse 不會清除 Dirty Bit，導致 Minor GC 掃描已釋放物件

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要物件被釋放後 slot 被回收再利用 |
| **Severity (嚴重程度)** | Medium | 導致Minor GC時錯誤掃描新配置的物件 |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap.rs` slot reuse, `gc.rs` dirty page scanning
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當 slot 被回收再利用時，髒標誌 (dirty bit) 應該被清除。這是因为：
1. 舊物件在舊世代中，可能因為 write barrier 設置了 dirty bit
2. 舊物件被釋放，slot 加入空閒列表
3. 新物件配置在同一個 slot
4. 新物件不應該繼承舊物件的 dirty bit

### 實際行為 (Actual Behavior)

當 slot 被回收再利用時，髒標誌沒有被清除 (`heap.rs:2209-2218`)：
```rust
// Clear DEAD_FLAG, GEN_OLD_FLAG, and UNDER_CONSTRUCTION_FLAG so reused slot is not
// incorrectly marked. (UNDER_CONSTRUCTION_FLAG can be set by Gc::new_cyclic_weak.)
// SAFETY: obj_ptr points to a valid GcBox slot (was in free list).
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
}
// 缺少: (*gc_box_ptr).clear_dirty();
```

這導致在 Minor GC 期間，`scan_dirty_page_minor` 和 `scan_dirty_page_minor_trace` 會錯誤地掃描新配置的物件。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs:2209-2218`：

當 slot 被重用時，只清除了以下 flag：
- `DEAD_FLAG`
- `GEN_OLD_FLAG`
- `UNDER_CONSTRUCTION_FLAG`

但**沒有**清除 `dirty` bit！

在 `gc.rs:1084` 中：
```rust
if (*header).is_dirty(i) {
    // 會掃描這個物件，即使它是一個新配置的物件
    ...
}
```

這會導致：
1. 舊物件在舊世代中，因為 OLD→YOUNG 引用設置了 dirty bit
2. 舊物件被釋放
3. Slot 被新物件重用（可能是年輕物件）
4. Dirty bit 沒有被清除
5. Minor GC 會錯誤地掃描這個「髒」物件

注意：對於大型物件 (large objects)，當配置新的大型物件時，整個頁面是freshly allocated的，所以 dirty bit 會是0（見 `heap.rs:2412-2414`）。這個問題只影響小型物件。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 配置一個舊世代的物件，並設置 OLD→YOUN G 引用（這會設置 dirty bit）
2. 釋放舊物件（slot 加入空閒列表）
3. 在同一個 slot 配置新物件
4. 執行 Minor GC (`collect()`)
5. 觀察新物件是否被錯誤地當作「髒」物件處理

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `heap.rs:2217` 後添加清除 dirty bit 的邏輯：

```rust
// Clear DEAD_FLAG, GEN_OLD_FLAG, UNDER_CONSTRUCTION_FLAG, and DIRTY_FLAG 
// so reused slot is not incorrectly marked.
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
    // 新增：清除髒標誌
    (*header).clear_dirty(idx);
}
```

或者，在 `PageHeader` 級別添加清除 dirty bit 的函數並調用它。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 generational GC 的基本正確性問題。Dirty bit 的目的是追蹤 OLD→YOUNG 引用。如果 slot 被重用時 dirty bit 沒有被清除，Minor GC 會錯誤地掃描新物件。

**Rustacean (Soundness 觀點):**
這可能導致內存安全問題。如果新物件被錯誤地當作「髒」物件處理，可能導致不正確的追蹤行為。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以觸發此 bug 來影響 GC 行為，可能導致內存洩漏或其他問題。

---

## Resolution Note (2026-03-13)

**Outcome:** Fixed and verified.

The fix is already implemented in `heap.rs`. In `try_pop_from_page` (lines 2217–2227), when a slot is reused from the free list, the code now clears the dirty bit via `(*header).clear_dirty(idx as usize)` alongside `clear_dead`, `clear_gen_old`, and `clear_under_construction`. The comment explicitly references bug122. No further code changes required.
