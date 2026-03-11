# [Bug]: record_satb_old_value 記錄已釋放物件 - 缺少 is_allocated 檢查

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 在對象被 sweep 後、但在引用被覆蓋前觸發 barrier |
| **Severity (嚴重程度)** | `High` | 可能導致 GC 嘗試追蹤已釋放記憶體，造成不確定行為 |
| **Reproducibility (復現難度)** | `Medium` | 需要精確時序控制，或可通過單執行緒重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::record_satb_old_value()` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`record_satb_old_value()` 應該只記錄有效的（已分配的）GC 指針。在記錄前應檢查 `is_allocated` 以確保對象未被釋放。

### 實際行為 (Actual Behavior)
當 incremental marking 啟用時，`GcCell::borrow_mut()` 等方法會呼叫 `record_satb_old_value()` 記錄舊的 GC 指針。但 `record_satb_old_value` 沒有檢查 `is_allocated`，直接將指標推入 SATB buffer。即使對象已被 sweep（釋放），只要 `allocating_thread_id != 0`，就會被記錄。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs` 的 `record_satb_old_value` 函數 (lines 1932-1947)：

```rust
pub fn record_satb_old_value(&mut self, gc_box: NonNull<GcBox<()>>) -> bool {
    let current_thread_id = get_thread_id();
    let allocating_thread_id = unsafe { get_allocating_thread_id(gc_box.as_ptr() as usize) };

    // Bug: 當 allocating_thread_id != 0 時，假設對象有效，沒有檢查 is_allocated
    if current_thread_id != allocating_thread_id && allocating_thread_id != 0 {
        Self::push_cross_thread_satb(gc_box);
        return true;
    }

    // 這裡也沒有檢查 is_allocated！
    self.satb_old_values.push(gc_box);
    if self.satb_old_values.len() >= self.satb_buffer_capacity {
        self.satb_buffer_overflowed()
    } else {
        true
    }
}
```

`get_allocating_thread_id` 在 release build 中不回傳 0，即使對象已被 sweep：
- 如果對象在 heap 範圍內且有 owner_thread，會返回該 thread ID
- 只有當地址在 heap 範圍外時才返回 0

因此，當對象被 sweep 後：
1. 地址仍在 heap 範圍內
2. `get_allocating_thread_id` 返回非 0 值（舊的 owner_thread）
3. `record_satb_old_value` 假設對象有效，推入 SATB buffer
4. GC 後續可能嘗試追蹤已釋放的記憶體

對比其他 write barrier 函數：
- `simple_write_barrier` (bug212) 有 `is_allocated` 檢查
- `incremental_write_barrier` (bug220) 有 `is_allocated` 檢查
- `GcThreadSafeCell::incremental_write_barrier` (bug221) 有 `is_allocated` 檢查

但 `record_satb_old_value` 缺少此檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 創建 GC 對象 A
2. 將 A 存入 GcCell
3. 觸發 GC（sweep）使 A 被釋放
4. 在 GcCell 上呼叫 `borrow_mut()` 覆蓋引用
5. 觀察 record_satb_old_value 是否記錄了已釋放的指標

或者使用 minor GC：
1. 創建對象 A，promote 到 old gen
2. 呼叫 `collect()` 進行 minor GC（只 sweep young generation）
3. 在 GcCell 中覆蓋引用
4. 檢查 SATB buffer 是否包含無效指標

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `record_satb_old_value` 中添加 `is_allocated` 檢查：

```rust
pub fn record_satb_old_value(&mut self, gc_box: NonNull<GcBox<()>>) -> bool {
    let gc_box_addr = gc_box.as_ptr() as usize;
    let current_thread_id = get_thread_id();
    let allocating_thread_id = unsafe { get_allocating_thread_id(gc_box_addr) };

    // 新增：檢查對象是否已分配
    if allocating_thread_id == 0 {
        return true; // 無效對象，不記錄
    }

    // 檢查 is_allocated（類似其他 write barrier）
    if let Some(idx) = unsafe { ptr_to_object_index(gc_box_addr as *const u8) } {
        let header = unsafe { ptr_to_page_header(gc_box_addr as *const u8) };
        if !unsafe { (*header.as_ptr()).is_allocated(idx) } {
            return true; // 對象已釋放，不記錄
        }
    }

    if current_thread_id != allocating_thread_id {
        Self::push_cross_thread_satb(gc_box);
        return true;
    }

    self.satb_old_values.push(gc_box);
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB barrier 的目的是確保標記開始時可達的對象保持可達。如果記錄已釋放的對象，GC 可能嘗試訪問無效記憶體，破壞標記完整性。這與其他 write barrier（如 incremental_write_barrier）檢查 is_allocated 的原因相同。

**Rustacean (Soundness 觀點):**
記錄已釋放的對象不會直接導致 UAF（因為 gc_box 記憶體可能仍然有效），但會導致不確定的 GC 行為。這是防禦性編程問題，需要與其他 write barrier 保持一致的檢查。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用釋放後使用的對象進行攻擊。雖然 SATB buffer 中的指標不會直接執行，但異常的 GC 行為可能為其他攻擊打開大門。
