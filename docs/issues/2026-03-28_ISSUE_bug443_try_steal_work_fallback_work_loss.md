# [Bug]: Work loss in `try_steal_work` fallback when overflow clearing is in progress

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | High contention GC with overflow queue clearing |
| **Severity (嚴重程度)** | Critical | Objects not marked, premature reclamation, use-after-free |
| **Reproducibility (復現難度)** | Medium | Requires concurrent clearing and work redistribution |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `try_steal_work` in `marker.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
When `push_overflow_work` fails during clearing, the work should either be retained or handled gracefully without loss.

### 實際行為
In `try_steal_work` (marker.rs:1184-1188), when the fallback `push_overflow_work(obj)` fails due to clearing in progress:
- The error is silently discarded with `let _ =`
- The comment claims "the work will be picked up by the clearer"
- But `clear_overflow_queue` only drains the queue via `pop_overflow_work()` - it cannot recover work from failed pushes
- **Work is permanently lost**

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Location:** `crates/rudo-gc/src/gc/marker.rs:1184-1188`

```rust
// Fallback: push back to overflow queue to prevent work loss
// This can only fail if clearing is in progress, in which case
// the work will be picked up by the clearer
let _ = push_overflow_work(obj);
return true;
```

The comment is incorrect. When `push_overflow_work` returns `Err(work)` (line 96), the work is returned to the caller, which discards it with `let _ =`. The clearer cannot pick up this work because:
1. `clear_overflow_queue` (line 175-195) only calls `pop_overflow_work()` to drain
2. Failed `push_overflow_work` returns work to the calling thread, not the queue
3. The work is dropped

**Race scenario:**
1. Thread A pops from overflow (succeeds)
2. Thread B starts clearing (increments clear_gen to odd)
3. Thread A tries fallback `push_overflow_work`
4. Thread A sees odd gen, spins until even, then returns `Err(obj)`
5. Thread A discards `Err(obj)` - **work lost**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Conceptual - requires controlling GC timing
// 1. Have workers processing overflow work
// 2. Trigger overflow queue clear during high contention
// 3. Observe objects not being traced
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Handle the error properly instead of discarding it. Options:
1. Loop until push succeeds (busy-wait)
2. Return the error to caller
3. Store in a thread-local buffer for later retry

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Work loss during marking is catastrophic - unmarked objects are collected. The fallback mechanism provides defense in depth but the error handling is broken.

**Rustacean (Soundness 觀點):**
Silent error discarding in unsafe GC code is dangerous. The `let _ =` pattern hides a critical failure mode.

**Geohot (Exploit 觀點):**
An attacker could time clearing operations to cause work loss, leading to use-after-free vulnerabilities.

---

## Resolution (2026-03-28)

**Fixed in code:** `try_steal_work` no longer discards `Err` from `push_overflow_work`. Both fallback sites (after pop from overflow and after steal) use `while push_overflow_work(obj).is_err() { spin_loop(); }` so work is retried until the push succeeds after `clear_overflow_queue` finishes. Matches suggested remediation (busy-wait until push succeeds). `test_try_steal_work_checks_overflow_queue` passes (`cargo test -p rudo-gc --all-features try_steal --lib`).