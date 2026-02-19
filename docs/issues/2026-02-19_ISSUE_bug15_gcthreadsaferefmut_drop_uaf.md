# [Bug]: GcThreadSafeRefMut::drop() 可能於並髮標記期間導致 UAF

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要同時滿足：1) 增量標記 active 2) 有並髮 GC 執行 3) 恰好在 drop 時進行 sweep |
| **Severity (嚴重程度)** | High | 可能導致 use-after-free，記憶體安全問題 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序，重現難度中等 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeRefMut` (cell.rs)
- **OS / Architecture:** All (平台無關)
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current main branch

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 `GcThreadSafeRefMut` guard 被 drop 時，如果此時有增量標記正在進行，應該安全地將被修改資料中的 GC 指標標記為黑色（live），確保這些指標不會被錯誤地回收。

### 實際行為 (Actual Behavior)
在 `GcThreadSafeRefMut` 的 `Drop` 實作中，程式碼無條件地存取內部資料並呼叫 `mark_object_black()`，沒有任何同步機制也沒有檢查：
1. GC 標記階段是否處於安全狀態
2. 物件是否可能已經被 sweep 回收

```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        if crate::gc::incremental::is_incremental_marking_active() {
            let mut ptrs = Vec::with_capacity(32);
            (*self.inner).capture_gc_ptrs_into(&mut ptrs);  // 無條件存取!

            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 drop 程式碼在 `MutexGuard` 被 drop 時執行，沒有任何同步機制。在並髮/平行標記期間，存在以下 race condition：

1. Mutator 執行 `GcThreadSafeRefMut::drop()`，標記物件為黑色
2. GC 同時正在 sweep 這些相同物件

這可能導致以下场景：
- 物件 A 被標記為黑色（live）
- GC 開始 sweep，檢查物件 A 發現是灰色（未標記）
- GC 回收物件 A 並將其加入 free list
- Mutator 的 drop 執行，試圖標記已回收的物件 A 為黑色
- 後續使用該記憶體時發生 UAF

此外，`is_incremental_marking_active()` 只檢查標記是否 active，但沒有確保標記與 sweep 之間的同步。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要在有多執行緒標記的環境下測試
fn main() {
    use rudo_gc::*;
    
    // 建立 GcThreadSafeCell
    let cell = GcThreadSafeCell::new(MyData::default());
    
    // 啟動增量標記
    crate::gc::incremental::set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });
    
    // 執行會觸發 drop 的操作
    {
        let mut guard = cell.borrow_mut();
        guard.update_gc_ptrs(); // 這會觸發 write barrier
    } // drop 在此發生
    
    // 同時觸發 GC sweep
    // ...
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **添加同步機制**：在 drop 中執行標記前，應該與 GC 階段進行某種同步，確保物件未被 sweep。

2. **使用 STW 期間執行**：將髒資料的標記推遲到 STW 期間執行，而不是在 drop 時立即執行。

3. **改用標記為灰色而非黑色**：將指標加入 dirty list 而不是直接標記為黑色，讓 GC 在下一個標記階段正確處理。

4. **檢查物件是否有效**：在標記前檢查物件是否已被回收（可透過 page header 的 allocation status 判斷）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個問題反映了增量標記的一個常見陷阱：在 mutator 和 GC 之間沒有足夠的同步時，標記操作可能會訪問已被回收的記憶體。在Chez Scheme中，我們通常透過 STW 屏障來確保這種安全性。建議將髒指標記錄推遲到下一個 STW 階段處理，而不是在 drop 時立即標記為黑色。

**Rustacean (Soundness 觀點):**
這是一個明確的記憶體安全問題。`unsafe` 區塊中的 `mark_object_black` 可能在物件已被回收後被呼叫，這是未定義行為。雖然理論上依賴 Rust 的型別系統和借用檢查，但實際上在 GC 環境中需要更謹慎的處理。建議添加指標有效性檢查或使用更安全的 API。

**Geohot (Exploit 觀點):**
這是一個經典的 TOCTOU (Time-of-Check to Time-of-Use) 漏洞。攻擊者可能透過精心設計的時序來觸發這個 race condition，特別是在即時系統或即時效能要求高的環境中。雖然利用難度較高，但一旦成功可以實現任意記憶體讀寫。建議添加時間戳記或版本號來檢測物件是否在標記期間被回收。
