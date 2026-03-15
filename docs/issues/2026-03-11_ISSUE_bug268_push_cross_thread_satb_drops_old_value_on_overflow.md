# [Bug]: push_cross_thread_satb 緩衝區溢位時丟棄舊值導致潛在 Use-After-Free

**Status:** Invalid
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要跨執行緒 GC 指針修改情境，且跨執行緒 SATB 緩衝區達到上限 (1M 條目) |
| **Severity (嚴重程度)** | High | 物件可能因未被正確記錄而被錯誤回收，導致 Use-After-Free |
| **Reproducibility (重現難度)** | Medium | 需要精確控制時序，使跨執行緒緩衝區溢出 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::push_cross_thread_satb()` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

當跨執行緒 SATB 緩衝區達到上限 (`MAX_CROSS_THREAD_SATB_SIZE` = 1M 條目) 時，`push_cross_thread_satb` 函數會：
1. 請求 fallback
2. **直接返回，丟棄應該被記錄的舊值**

這與 bug248（返回值不一致）不同，這是一個更嚴重的問題：舊值被丟棄可能導致物件在標記階段被錯誤地視為死亡，進而導致 Use-After-Free。

### 預期行為
當緩衝區溢出時，應該：
1. 請求 fallback
2. 仍然記錄舊值（或採用其他機制確保物件存活）

### 實際行為
函數直接返回，舊值被丟棄，導致該物件可能在 GC 標記階段被錯誤回收。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs:1963-1971`：

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        return;  // <-- BUG: 舊值被丟棄!
    }
    buffer.push(gc_ptr.as_ptr() as usize);
}
```

相比之下，正常路徑 (`satb_buffer_overflowed`) 會將現有緩衝區內容合併到 overflow buffer 確保不丟失資料：

```rust
fn satb_buffer_overflowed(&mut self) -> bool {
    self.satb_overflow_buffer.append(&mut self.satb_old_values);  // 保留舊值
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟用 incremental marking feature
2. 在一個執行緒中建立 OLD 世代的 GC 物件
3. 多個其他執行緒同時修改跨執行緒 GC 指針
4. 使 CROSS_THREAD_SATB_BUFFER 超過 1M 條目
5. 觀察 OLD 物件指向的 young 物件是否被錯誤回收

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：始終記錄值，即使請求了 fallback

```rust
pub fn push_cross_thread_satb(gc_ptr: NonNull<GcBox<()>>) {
    let mut buffer = CROSS_THREAD_SATB_BUFFER.lock();
    if buffer.len() >= MAX_CROSS_THREAD_SATB_SIZE {
        crate::gc::incremental::IncrementalMarkState::global()
            .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
        // 不要 return，繼續記錄以確保物件存活
    }
    buffer.push(gc_ptr.as_ptr() as usize);
}
```

選項 2：將溢出的值添加到 fallback 機制

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB (Snapshot-At-The-Beginning) 的核心不變性是「在 GC 開始時存活的物件，必須被視為存活」。當舊值被丟棄時，這個不變性被破壞。增量標記依賴於這些舊值來確保不會錯誤回收物件。雖然 fallback 機制會觸發完整 GC，但兩次 GC 之間的視窗中，物件可能被錯誤回收。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當物件被錯誤回收後，如果還有指標指向該記憶體位置（因為是 unsafe 指標或其他原因），就會產生 Use-After-Free。這比 bug248（返回值不一致）更嚴重，因為影響的是實際的記憶體安全性。

**Geohot (Exploit 觀點):**
在極端並發情況下，攻擊者可以通過使跨執行緒 SATB 緩衝區溢出來觸發此 bug。雖然觸發條件較高（需要 1M 條目），但一旦觸發，可能導致記憶體錯誤利用。這個問題比返回值不一致更嚴重，因為它直接影響物件生命週期。

---

## ✅ 驗證記錄 (Verification Record)

### 2026-03-11 驗證
- **驗證結果**: Bug 存在於目前程式碼中
- **程式碼位置**: `crates/rudo-gc/src/heap.rs:1963-1971`
- **確認事項**:
  - 當緩衝區滿時，函數請求 fallback 後直接返回 (line 1965-1968)
  - 未將舊值添加到任何 fallback 機制
  - 與正常路徑的 `satb_buffer_overflowed` 行為不同，後者會保留舊值
- **影響**: 此 bug 可能導致 Use-After-Free

---

## Resolution (2026-03-15)

**Outcome:** Invalid — superseded by bug122 and bug270.

The codebase has evolved since this issue was filed. The original scenario (main buffer full → drop value) was fixed by bug122: `CROSS_THREAD_SATB_OVERFLOW_BUFFER` was added. When the main buffer is full, pointers are now pushed to the overflow buffer instead of being dropped.

When **both** main and overflow buffers are full (each capped at 1M per bug270), the implementation intentionally drops the value and requests fallback to prevent unbounded growth (OOM). This is a deliberate trade-off documented in bug270's resolution. The suggested fix in this issue ("始終記錄值") would reintroduce bug270's unbounded growth. No code change.
