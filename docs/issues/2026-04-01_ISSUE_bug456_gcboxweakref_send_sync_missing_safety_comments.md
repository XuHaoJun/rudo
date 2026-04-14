# [Bug]: GcBoxWeakRef Send/Sync impls missing SAFETY comments

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | 所有 GcBoxWeakRef 使用都受影響 |
| **Severity (嚴重程度)** | `Low` | 文件缺失，不影響功能正確性 |
| **Reproducibility (重現難度)** | `N/A` | 文件問題，無需重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef` Send/Sync impls (ptr.rs:1028-1031)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`unsafe impl` 區塊應該包含 SAFETY 註解，說明為何此實作是安全的。

### 實際行為 (Actual Behavior)
`GcBoxWeakRef<T>` 的 Send 和 Sync 實作缺少 SAFETY 註解：
```rust
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + 'static> Send for GcBoxWeakRef<T> {}
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: Trace + 'static> Sync for GcBoxWeakRef<T> {}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`GcBoxWeakRef<T>` 結構：
```rust
pub(crate) struct GcBoxWeakRef<T: Trace + 'static> {
    ptr: AtomicNullable<GcBox<T>>,
    generation: u32,
}
```

所有欄位都是 Send + Sync：
- `AtomicNullable<GcBox<T>>` - 原子指標，Send + Sync
- `generation: u32` - plain u32，Send + Sync

因此 `GcBoxWeakRef<T>` 應該是 Send + Sync，但缺少 SAFETY 註解來記錄這個不變性。

`#[allow(clippy::non_send_fields_in_send_ty)]` 表示 clippy 對此實作有疑慮，但沒有文件說明為何這是安全的。

---

## 💣 重現步驟 / 概念驗證 (PoC)

不適用 - 這是文件問題，不需要 PoC。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 ptr.rs:1028-1031 添加 SAFETY 註解：

```rust
// SAFETY: GcBoxWeakRef<T> is Send + Sync because:
// - ptr: AtomicNullable<GcBox<T>> is Send + Sync (atomic pointer)
// - generation: u32 is Send + Sync (plain u32)
// - T: Trace + 'static bound ensures no non-Send/Sync types in the generic
unsafe impl<T: Trace + 'static> Send for GcBoxWeakRef<T> {}
unsafe impl<T: Trace + 'static> Sync for GcBoxWeakRef<T> {}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GC weak reference 結構通常需要確保跨執行緒安全。AtomicNullable 提供了必要的同步。

**Rustacean (Soundness 觀點):**
缺少 SAFETY 註解違反了專案的程式碼規範（AGENTS.md：「All unsafe code must have `// SAFETY:` comments」）。

**Geohot (Exploit 觀點):**
文件缺失降低了安全性審計的有效性。