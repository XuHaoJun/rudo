# [Bug]: dec_weak uses Relaxed ordering causing stale weak_count reads

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Race condition requires specific thread interleaving |
| **Severity (嚴重程度)** | High | Could cause premature reclamation or reference counting errors |
| **Reproducibility (重現難度)** | Medium | Requires concurrent weak reference operations |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::dec_weak()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

### 預期行為
`dec_weak` 應該使用 `Acquire` ordering 來載入 `weak_count`，確保與其他執行緒的寫入正確同步。

### 實際行為
`dec_weak` 使用 `Relaxed` ordering 載入 `weak_count`（line 397），與同檔案中其他類似函式不一致。

---

## 🔬 根本原因分析 (Root Cause Analysis)

位於 `crates/rudo-gc/src/ptr.rs:397`：

```rust
pub fn dec_weak(&self) -> bool {
    loop {
        let current = self.weak_count.load(Ordering::Relaxed);  // BUG: 應為 Acquire
        // ...
    }
}
```

對比其他類似函式：
- `try_inc_ref_from_zero` (line 296-297): 使用 `Acquire`
- `dec_ref` (line 185): 使用 `Acquire`

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Weak reference count 的正確同步對 GC 的 cyclic reference 處理至關重要。使用 `Relaxed` 可能導致執行緒看到過時的 weak_count 值，進而錯誤地回收仍有 weak reference 存活的物件。

**Rustacean (Soundness 觀點):**
這不是傳統的 memory safety bug，但可能導致 reference counting 不正確，進而造成 use-after-free 或 double-free。

**Geohot (Exploit 觀點):**
如果攻擊者能控制 concurrent weak reference 操作，可能利用這個 race condition 觸發不正確的記憶體回收。