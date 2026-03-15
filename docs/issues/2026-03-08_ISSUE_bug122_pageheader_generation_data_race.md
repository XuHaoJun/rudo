# [Bug]: PageHeader.generation data race between GC and mutator

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent GC and mutator access to same page |
| **Severity (嚴重程度)** | `High` | Data race is UB; can cause incorrect barrier decisions |
| **Reproducibility (復現難度)** | `High` | Needs concurrent GC and specific timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `PageHeader.generation`, write barrier, generational GC
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The `generation` field in `PageHeader` should be accessed atomically since it is written by GC thread and read by mutator threads concurrently.

### 實際行為 (Actual Behavior)
The `generation` field is a plain `u8` (non-atomic), creating a data race:
- **Write**: GC thread writes `generation = 1` in `promote_young_pages()` (gc.rs:1692) and `promote_all_pages()` (gc.rs:2327)
- **Read**: Mutator reads `generation` in `record_page_in_remembered_buffer()` (cell.rs:303) during write barrier execution

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `PageHeader` struct defines `generation: u8` (heap.rs:935) as a plain u8, but it is accessed concurrently:
1. GC thread promotes pages by setting `generation = 1`
2. Mutator threads read `generation` in write barrier to decide if page needs remembered buffer recording
3. No synchronization primitives protect these concurrent accesses

This is undefined behavior per Rust's memory model - data races on non-atomic, non-Sync types.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This would require Miri or ThreadSanitizer to detect
// The race occurs between:
// - GC thread: (*header).generation = 1 (gc.rs:1692, 2327)
// - Mutator: (*header).generation > 0 (cell.rs:303)
//
// Concurrent minor GC while mutator performs write barriers
// could trigger the data race.
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. Change `generation: u8` to `generation: AtomicU8` in `PageHeader` (heap.rs:935)
2. Use `.store(1, Ordering::SeqCst)` when writing in `promote_young_pages()` and `promote_all_pages()`
3. Use `.load(Ordering::SeqCst)` when reading in `record_page_in_remembered_buffer()` and other locations

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic concurrent GC issue. The generation field tracks page age for generational barriers. If the mutator reads stale/wrong generation values, it may incorrectly skip recording old→young references in the remembered buffer, leading to young objects being prematurely collected.

**Rustacean (Soundness 觀點):**
This is undefined behavior. Data races on non-atomic types violate Rust's memory model. The compiler may optimize assuming this never happens, potentially causing memory corruption or incorrect program behavior. Should be fixed with `AtomicU8`.

**Geohot (Exploit 觀點):**
While this is primarily a correctness issue, the timing-dependent nature could potentially be exploited in real-time systems where GC timing is predictable. However, the more immediate concern is the UB itself which could manifest as arbitrary memory corruption.
