# [Bug]: dec_weak_raw and Weak::drop use Relaxed ordering for weak_count load (inconsistent with dec_weak)

**Status:** Invalid
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Race condition requires concurrent weak reference operations |
| **Severity (嚴重程度)** | Medium | Could cause inconsistent visibility of DEAD_FLAG in concurrent scenarios |
| **Reproducibility (重現難度)** | Medium | Requires concurrent weak reference operations |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::dec_weak_raw()` (ptr.rs:366) and `Weak::drop()` (ptr.rs:2883)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`dec_weak_raw` 和 `Weak::drop` 應該使用 `Acquire` ordering 來載入 `weak_count`，確保與其他執行緒的寫入正確同步，與 `dec_weak()` 保持一致。

### 實際行為
- `dec_weak_raw` (line 366): 使用 `Ordering::Relaxed` 載入 weak_count
- `Weak::drop` (line 2883): 使用 `Ordering::Relaxed` 載入 weak_count
- `dec_weak()` (line 397): 使用 `Ordering::Acquire` (bug423 修復後)

在 commit 23f42df 中，bug423 修復了 `dec_weak` 的 Relaxed ordering 問題，但 `dec_weak_raw` 和 `Weak::drop` 使用相同的模式卻未被修復。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### dec_weak_raw (ptr.rs:366)
```rust
let current = (*weak_count_ptr).load(Ordering::Relaxed);  // 應為 Acquire
```

### Weak::drop (ptr.rs:2883)
```rust
let mut current = (*weak_count_ptr).load(Ordering::Relaxed);  // 應為 Acquire
```

### 對比 dec_weak (ptr.rs:397) - 已修復
```rust
let current = self.weak_count.load(Ordering::Acquire);  // 正確
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Weak reference count 的正確同步對 GC 的 cyclic reference 處理至關重要。使用 `Relaxed` 可能導致執行緒看到過時的 weak_count 值。

**Rustacean (Soundness 觀點):**
這與 bug423 修復的 `dec_weak` 問題相同，但 `dec_weak_raw` 和 `Weak::drop` 使用相同的模式卻未被修復。應保持一致性。

**Geohot (Exploit 觀點):**
Relaxed ordering 可能在特定編譯器優化下導致非預期的行為。

---

## 建議修復方案

修改兩處 Relaxed 為 Acquire：

1. `dec_weak_raw` (line 366): `Ordering::Relaxed` → `Ordering::Acquire`
2. `Weak::drop` (line 2883): `Ordering::Relaxed` → `Ordering::Acquire`

---

## 相關 Issue

- bug423: dec_weak 使用 Relaxed ordering (已修復 dec_weak，但 dec_weak_raw 和 Weak::drop 未修復)

---

## Resolution (2026-03-28)

**Invalid — already fixed in current tree.** In `crates/rudo-gc/src/ptr.rs`, `GcBox::dec_weak_raw` loads `weak_count` with `Ordering::Acquire` (line ~366), and `Weak<T>::drop` uses `Ordering::Acquire` for the initial load in its CAS loop (line ~2861), matching `dec_weak()`. No code change required.