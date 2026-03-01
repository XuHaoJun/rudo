# [Bug]: GcHandle::clone 存在 TOCTOU Race Condition - origin_thread 檢查與使用之間存在時間窗口

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Rare | 需要精確的執行緒終止時機，較難觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致執行緒檢查失效，但不會直接造成 UAF |
| **Reproducibility (復現難度)** | Very High | 需要精確控制執行緒終止時機 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::clone()`, `handles/cross_thread.rs:345-353`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcHandle::clone()` 應該在整個操作過程中一致地檢查執行緒親和性。如果 origin 執行緒處於活躍狀態，應該阻止從其他執行緒克隆。

### 實際行為 (Actual Behavior)
在 `GcHandle::clone()` 中，`origin_tcb.upgrade()` 被調用兩次：
1. 第一次（line 345）：`if self.origin_tcb.upgrade().is_some()` - 檢查 origin 執行緒是否存活
2. 第二次（line 353）：`self.origin_tcb.upgrade().map_or_else(...)` - 使用結果

這兩次調用之間存在時間窗口，如果 origin 執行緒在第一次檢查後、第二次調用前終止，會導致：
- 第一次檢查返回 `true`（執行緒還活著）
- 執行緒在窗口期間終止
- 第二次調用返回 `None`（變成 orphan）

### 程式碼位置
```rust
// Line 345: 第一次檢查
if self.origin_tcb.upgrade().is_some() {
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        ...
    );
}
// === 時間窗口：origin 執行緒可能在這裡終止 ===
// Line 353: 第二次調用
let (new_id, origin_tcb) = self.origin_tcb.upgrade().map_or_else(
    ...
);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **TOCTOU (Time-Of-Check-Time-Of-Use) 漏洞**：在 line 345 進行執行緒檢查後，沒有保證後續操作（line 353）使用的狀態與檢查時相同。

2. **並發控制不足**：`origin_tcb.upgrade()` 的結果在兩個獨立的語句中使用，之間沒有同步機制。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此漏洞需要精確的時機控制才能穩定重現：

1. 創建一個 GcHandle
2. 在一個單獨的執行緒中調用 clone()
3. 同時讓 origin 執行緒終止
4. 觀察執行緒檢查是否被錯誤地跳過

理論上可能的場景：
```rust
// Thread A (origin)
let gc = Gc::new(Data { value: 42 });
let handle = gc.cross_thread_handle();

// Thread B: 嘗試在 A 終止時克隆
std::thread::spawn(|| {
    // 如果 A 在這個精確時刻終止
    // clone() 的第一次 upgrade() 返回 Some
    // 第二次 upgrade() 返回 None
    // 導致執行緒檢查被錯誤地跳過
    let _ = handle.clone();
});
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**選項 1：單次升級 + 結構化分支**
```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        if self.handle_id == HandleId::INVALID {
            panic!("cannot clone an unregistered GcHandle");
        }
        
        // 單次升級，保存結果
        let origin_alive = self.origin_tcb.upgrade();
        
        // 使用結果進行檢查和分支
        if let Some(tcb) = origin_alive {
            // Origin 執行緒仍存活，檢查執行緒親和性
            assert_eq!(
                std::thread::current().id(),
                self.origin_thread,
                "GcHandle::clone() must be called on the origin thread."
            );
            
            // ... 使用 tcb 的分支邏輯
        } else {
            // Origin 已終止，使用 orphan 分支邏輯
            // ...
        }
    }
}
```

**選項 2：使用 OnceCell 快取結果**
```rust
// 在 GcHandle 結構中添加
struct GcHandle<T: Trace + 'static> {
    // ... existing fields
    cached_origin_alive: std::sync::OnceLock<bool>,
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此漏洞影響較小，因為即使執行緒檢查被錯誤地跳過，最終仍會通過 orphan root 機制處理克隆請求。不會導致記憶體不安全，只是 API 行為略有偏差。

**Rustacean (Soundness 觀點):**
這是一個邏輯漏洞而非記憶體安全漏洞。雖然不影響內存安全性，但破壞了 API 的一致性預期，可能導致調用者無法正確預測程式行為。

**Geohot (Exploit 觀點):**
此漏洞難以實際利用。需要極精確的時機控制才能在生產環境中觸發。但理論上可能作為輔助漏洞，與其他漏洞結合造成更大影響。

---

## Resolution (2026-03-02)

**Fixed.** Applied single-upgrade pattern: `origin_tcb.upgrade()` is now called once and the result stored; `map_or_else` operates on that stored value so check and use observe the same state. Verified via `cross_thread_handle` and `bug4_tcb_leak` tests.
