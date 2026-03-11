# [Bug]: Weak<T>::drop 使用 Relaxed 載入指標導致潛在記憶體洩漏

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發存取且物件正在銷毀時觸發 |
| **Severity (嚴重程度)** | Medium | 導致 weak reference count 無法正確遞減，造成記憶體洩漏 |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒時序才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為
`Weak<T>::drop` 應該使用 `Ordering::Acquire` 載入指標，確保讀取到最新狀態後再檢查 flags（dead flag、dropping state、under-construction），與 `Gc<T>::drop` 保持一致。

### 實際行為
`Weak<T>::drop` 在 line 2250 使用 `Ordering::Relaxed` 載入指標，但隨後在 lines 2263-2266 檢查可能被其他執行緒修改的 flags。這導致讀取到過時的指標/狀態。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:2250`：
```rust
let ptr = self.ptr.load(Ordering::Relaxed);
```

而 `Gc<T>::drop` 在 line 1709 正確使用：
```rust
let ptr = self.ptr.load(Ordering::Acquire);
```

使用 `Relaxed` 順序載入指標會造成以下問題：
1. **過時的 flag 讀取**：另一個執行緒可能在 `Weak<T>::drop` 執行期間設定 dead flag、dropping state 或 under-construction flag
2. **記憶體洩漏**：如果讀取到過時資料（表示物件已死/正在dropping/建構中，但實際上不是），會跳過遞減 weak count。這會導致 weak reference count 永久錯誤偏高，GcBox 永遠無法完全清理
3. **與 Gc<T> 不一致**：相同操作在 `Gc<T>::drop` 正確使用 `Acquire` 順序

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發 PoC 才能穩定重現。單執行緒測試無法觸發此問題。

```rust
// 需要多執行緒並發測試
// 執行緒 1: 持有 Weak<T> 即將 drop
// 執行緒 2: 同時修改同一 GcBox 的 dropping_state 或 dead flag
// 預期: Weak count 正確遞減
// 實際: 可能跳過遞減，導致記憶體洩漏
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `ptr.rs:2250` 的 `Ordering::Relaxed` 改為 `Ordering::Acquire`：

```rust
let ptr = self.ptr.load(Ordering::Acquire);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
使用 Relaxed ordering 載入指標後立即檢查 flag，违反了 memory barrier 的基本要求。在并发 GC 中，必须确保在检查对象状态前看到最新的指针值。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB（因為後續有 flag 檢查），但违反了 atomic 操作的 intent。使用 Relaxed 後馬上讀取其他 atomic flag，語義上不正確。

**Geohot (Exploit 觀點):**
精確時序依賴，但理論上可利用。攻擊者若能控制執行緒時序，可能導致目標應用程式記憶體洩漏。
