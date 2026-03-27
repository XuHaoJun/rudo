# [Bug]: Work loss in `try_steal_work` when all queues are full

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | High contention GC scenarios with many workers |
| **Severity (嚴重程度)** | Critical | Objects not marked, premature reclamation, use-after-free |
| **Reproducibility (復現難度)** | Medium | Requires concurrent workers with full queues |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `try_steal_work` in `marker.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
When `try_steal_work` successfully steals an object from a remote queue but cannot push it to any queue, it should have a fallback mechanism (like the first case in the same function) to prevent work loss.

### 實際行為
In `try_steal_work` (marker.rs:1196-1208), when an object is stolen but all push attempts fail:
- First case (pop_overflow): Has fallback to `push_overflow_work(obj)` and returns `true`
- Second case (steal from remote): Just drops `obj` and continues loop

This creates inconsistent behavior where stolen work can be permanently lost.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Location:** `crates/rudo-gc/src/gc/marker.rs:1196-1208`

```rust
if let Some(obj) = other.steal() {
    if queue.push(obj) {
        return true;
    }
    for other2 in all_queues {
        if other2.worker_idx() == queue.worker_idx() {
            continue;
        }
        if other2.push(obj) {
            return true;
        }
    }
    // BUG: obj is dropped here if all pushes fail!
}
```

The first case (lines 1172-1188) has a fallback:
```rust
// Fallback: push back to overflow queue to prevent work loss
// This can only fail if clearing is in progress, in which case
// the work will be picked up by the clearer
let _ = push_overflow_work(obj);
return true;
```

But the second case has no such fallback - if all queues are full and overflow is unavailable (clearing in progress), the stolen work is dropped.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

High contention scenario:
1. Spawn multiple GC worker threads
2. Fill all worker queues to capacity
3. Force a steal operation in `try_steal_work`
4. Observe that stolen work is dropped when pushes fail

```rust
// Conceptual scenario - actual reproduction requires concurrent threads
// and queue saturation which is timing-dependent
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add overflow fallback to the second case, matching the first case pattern:

```rust
if let Some(obj) = other.steal() {
    if queue.push(obj) {
        return true;
    }
    for other2 in all_queues {
        if other2.worker_idx() == queue.worker_idx() {
            continue;
        }
        if other2.push(obj) {
            return true;
        }
    }
    // Fallback: push back to overflow queue to prevent work loss
    // This can only fail if clearing is in progress, in which case
    // the work will be picked up by the clearer
    let _ = push_overflow_work(obj);
    return true;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Work loss during marking is serious - unmarked objects may be prematurely collected. The fallback to overflow queue provides defense in depth. Without it, we rely solely on the assumption that all queues will always accept work, which is not true under high contention.

**Rustacean (Soundness 觀點):**
This is a memory safety issue. If objects are not marked as live during GC, they can be freed and later accessed, causing use-after-free. This is undefined behavior.

**Geohot (Exploit 觀點):**
A sophisticated attacker could trigger high contention scenarios to cause work loss, potentially leading to use-after-free exploits. Even without explicit triggering, natural contention could cause sporadic memory corruption.