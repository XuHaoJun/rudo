# [Bug]: record_satb_old_value 返回值不一致 - 跨執行緒緩衝區溢位時仍返回 true

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要跨執行緒 GC 指針修改情境，且跨執行緒 SATB 緩衝區達到上限 (1M 條目) |
| **Severity (嚴重程度)** | High | 導致 caller 誤以為 SATB 記錄成功，實際上已請求 fallback，影響 GC 正確性 |
| **Reproducibility (重現難度)** | Medium | 需要精確控制時序，使跨執行緒緩衝區溢出 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::record_satb_old_value()` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`record_satb_old_value` 函數的文檔說明：
```
/// Returns `true` if the value was stored successfully, `false` if the buffer
/// overflowed and fallback was requested.
```

但實際實現與文檔不符。當執行緒 ID 與配置執行緒 ID 不同時（跨執行緒情境），函數調用 `push_cross_thread_satb` 後直接返回 `true`，即使 `push_cross_thread_satb` 內部已請求 fallback 也一樣。

### 預期行為
當跨執行緒 SATB 緩衝區溢出（達到 `MAX_CROSS_THREAD_SATB_SIZE` = 1M 條目）時，應該返回 `false`，告知 caller 需要 fallback。

### 實際行為
函數總是返回 `true`，即使 fallback 已被請求。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs` 的 `record_satb_old_value` 函數 (lines 1932-1947)：

```rust
pub fn record_satb_old_value(&mut self, gc_box: NonNull<GcBox<()>>) -> bool {
    let current_thread_id = get_thread_id();
    let allocating_thread_id = unsafe { get_allocating_thread_id(gc_box.as_ptr() as usize) };

    if current_thread_id != allocating_thread_id && allocating_thread_id != 0 {
        Self::push_cross_thread_satb(gc_box);
        return true;  // <-- 始終返回 true，未檢查是否請求了 fallback
    }

    self.satb_old_values.push(gc_box);
    if self.satb_old_values.len() >= self.satb_buffer_capacity {
        self.satb_buffer_overflowed()  // <-- 這裡正確返回 false
    } else {
        true
    }
}
```

而 `push_cross_thread_satb` 函數 (lines 1963-1971) 會在緩衝區滿時請求 fallback：

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        return;  // <-- 請求 fallback 但 caller 不知道
    }
    buffer.push(gc_ptr.as_ptr() as usize);
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟用 incremental marking feature
2. 多個執行緒同時修改跨執行緒 GC 指針
3. 使 CROSS_THREAD_SATB_BUFFER 超過 1M 條目
4. 觀察 `record_satb_old_value` 返回值是否仍為 `true`（預期應為 `false`）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `record_satb_old_value` 以檢查 fallback 請求狀態：

```rust
pub fn record_satb_old_value(&mut self, gc_box: NonNull<GcBox<()>>) -> bool {
    let current_thread_id = get_thread_id();
    let allocating_thread_id = unsafe { get_allocating_thread_id(gc_box.as_ptr() as usize) };

    if current_thread_id != allocating_thread_id && allocating_thread_id != 0 {
        Self::push_cross_thread_satb(gc_box);
        // 檢查是否請求了 fallback
        if crate::gc::incremental::IncrementalMarkState::global().fallback_requested() {
            return false;
        }
        return true;
    }

    self.satb_old_values.push(gc_box);
    if self.satb_old_values.len() >= self.satb_buffer_capacity {
        self.satb_buffer_overflowed()
    } else {
        true
    }
}
```

或者讓 `push_cross_thread_satb` 返回是否請求了 fallback：

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) -> bool {
    // 返回 true 表示成功，false 表示請求了 fallback
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        return false;
    }
    buffer.push(gc_ptr.as_ptr() as usize);
    true
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題影響 SATB (Snapshot-At-The-Beginning) 不變性。當 fallback 被請求但 caller 不知道時，GC 可能會錯誤地認為所有舊值都已被正確記錄，導致標記階段可能漏掉某些應該存活的物件。

**Rustacean (Soundness 觀點):**
這是 API 與合約不符的問題 - 函數文檔承諾返回 `false` 當 fallback 被請求，但實際實現從未返回 `false`（在跨執行緒路徑中）。這可能導致 caller 做出錯誤的假設。

**Geohot (Exploit 觀點):**
在極端並發情況下，攻擊者可能通過使跨執行緒 SATB 緩衝區溢出來觸發此 bug，導致 GC 行為異常。

---

## ✅ 驗證記錄 (Verification Record)

### 2026-03-10 再次驗證
- **驗證結果**: Bug 仍然存在於目前程式碼中
- **程式碼位置**: `crates/rudo-gc/src/heap.rs:1936-1938`
- **確認事項**:
  - `push_cross_thread_satb` 可在緩衝區滿時請求 fallback (line 1965-1968)
  - `record_satb_old_value` 在跨執行緒路徑中仍無條件返回 `true` (line 1938)
  - 未檢查 `fallback_requested()` 狀態
- **影響**: 此 bug 尚未修復
