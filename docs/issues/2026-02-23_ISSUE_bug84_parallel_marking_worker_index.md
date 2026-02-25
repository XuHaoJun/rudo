# [Bug]: Parallel Marking Worker Index Uses Wrong Pointer

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 每次 parallel minor/major GC 都會發生，但只影響效能 |
| **Severity (嚴重程度)** | Low | 不會導致 memory corruption，只影響效能 |
| **Reproducibility (復現難度)** | Medium | 需 benchmark 觀察 load imbalance |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Parallel Marking (`gc.rs`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在 parallel marking 中，worker index 應該根據實際 GC 物件指標 (`gc_box`) 計算，以實現均勻的工作分配。

### 實際行為 (Actual Behavior)
在 `mark_and_push_to_worker_queue` 函數中，worker index 使用 root 指針 (`ptr`) 而非 GC 物件指標 (`gc_box`) 計算。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/gc.rs:1229`：

```rust
let worker_idx = ptr as usize % num_workers;
worker_queues[worker_idx].push(gc_box.as_ptr());
```

函數接收兩個指標：
- `ptr: *const u8` - root 指針（棧上的 GC 指標位置）
- `gc_box: NonNull<GcBox<()>>` - 實際的 GC 物件

但計算 worker index 時使用了 `ptr` 而非 `gc_box`。這會導致：
1. **負載不均**：root 指針可能集中在某些地址範圍
2. **非確定性行為**：不同運行的 work distribution 可能不同

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此問題難以用簡單 PoC 重現，因為：
1. 這是效能問題，不會導致錯誤行為
2. 需使用 benchmark 工具觀察 worker load distribution
3. 可能在某些特定 stack layout 下更明顯

建議修復後使用自訂 benchmark 驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `crates/rudo-gc/src/gc/gc.rs:1229` 修改為：

```rust
let worker_idx = gc_box.as_ptr() as usize % num_workers;
```

或使用 hash 以獲得更均勻的分佈：

```rust
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

let mut hasher = DefaultHasher::new();
gc_box.as_ptr().cast::<()>().hash(&mut hasher);
let worker_idx = (hasher.finish() as usize) % num_workers;
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Parallel marking 的核心目標是通過 work stealing 實現負載均衡。使用 root 指針計算 worker index 完全違背了這個目標。Root 指針通常在 stack 上連續分布，導致多個 object 可能被分配到同一個 worker，造成其他 worker 閒置。這不是正確的 work distribution 策略。

**Rustacean (Soundness 觀點):**
這是效能 bug，不涉及 soundness 或 UB。代碼邏輯正確，只是選擇了錯誤的指標用於負載均衡。

**Geohot (Exploit 觀點):**
此問題無安全風險，純粹是效能優化問題。在某些極端情況下（如大量臨時對象在棧上），可能導致 GC 暂停時間增加。

---

## Resolution (2026-02-26)

**Outcome:** Already fixed.

The current implementation in `gc/gc.rs` line 1232 correctly uses `gc_box.as_ptr()` for worker index calculation:

```rust
let worker_idx = gc_box.as_ptr() as usize % num_workers;
```

The issue described using `ptr` (root pointer) instead of `gc_box`; the codebase already uses the correct pointer. No code changes required.
