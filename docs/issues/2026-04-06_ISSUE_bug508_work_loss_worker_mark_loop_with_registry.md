# [Bug]: work_mark_loop_with_registry drops work when all queues are full

**Status:** Fixed
**Tags:** Verified, Fixed
**Fixed by:** commit 4abc5a4 in gc/marker.rs (worker_mark_loop_with_registry)

## Resolution (2026-04-06)

**Outcome:** Fixed.

**Applied fix:** Added overflow queue fallback (lines 1185-1190) when all queue pushes fail:

```rust
// FIX bug508: Fallback to overflow queue to prevent work loss.
while push_overflow_work(obj).is_err() {
    std::hint::spin_loop();
}
```

**Verification:** Commit `4abc5a4` shows the fix was applied.

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Under high GC pressure, all queues may be full simultaneously |
| **Severity (嚴重程度)** | `High` | Objects may be incorrectly collected, causing use-after-free |
| **Reproducibility (復現難度)** | `Medium` | Requires concurrent marking with high object count |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/marker.rs::worker_mark_loop_with_registry`
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

In `worker_mark_loop_with_registry`, when a worker steals work via `try_steal_with_backoff` but fails to push it to any queue (all queues are full), the work item is silently dropped. This violates the "work is not lost" invariant.

### 預期行為 (Expected Behavior)
All work items must either be processed or returned to the overflow queue. No work should be lost.

### 實際行為 (Actual Behavior)
When all queues are full and the stolen object cannot be pushed anywhere, it is simply dropped - causing potential work loss and incorrect GC marking.

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `worker_mark_loop_with_registry` (lines 1170-1185):

```rust
if let Some(obj) = try_steal_with_backoff(&queue, all_queues) {
    if queue.push(obj) {
        continue;
    }
    // Push failed, try other queues
    for other in all_queues {
        if other.worker_idx() == queue.worker_idx() {
            continue;
        }
        if other.push(obj) {
            registry.notify_work_available();
            break;
        }
    }
    // BUG: If ALL pushes fail, obj is dropped - WORK LOSS!
}
```

The sibling function `try_steal_work` (lines 1195-1243) has the correct fallback:
```rust
// Fallback: push back to overflow queue to prevent work loss
while push_overflow_work(obj).is_err() {
    std::hint::spin_loop();
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// High concurrency scenario:
// 1. Multiple GC worker threads
// 2. Heavy marking workload
// 3. All queues become full simultaneously
// 4. Steal succeeds but all pushes fail
// Result: stolen object is dropped
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add overflow queue fallback after the failed push loop:

```rust
if other.push(obj) {
    registry.notify_work_available();
    break;
}
// FIX: Fallback to overflow queue to prevent work loss
while push_overflow_work(obj).is_err() {
    std::hint::spin_loop();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Work loss in marking is catastrophic - unmarked accessible objects will be swept, causing use-after-free. The overflow queue exists precisely to handle this case. The fix aligns with Chez Scheme's approach where overflow work is never lost.

**Rustacean (Soundness 觀點):**
If marked objects are dropped before being traced, they could be incorrectly swept, leading to UB when accessed later. This is a memory safety violation.

**Geohot (Exploit 觀點):**
Under high GC pressure, this bug creates a reliable way to cause use-after-free by controlling queue fullness.