# [Bug]: Global Mutexes Missing Lock Ordering Validation - Potential Deadlocks

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Lock ordering violations only cause issues when code acquires locks in wrong order. Currently no observed deadlocks. |
| **Severity (嚴重程度)** | High | Could cause permanent deadlock in production |
| **Reproducibility (復現難度)** | Very High | Would require specific lock acquisition ordering that doesn't currently exist |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Lock ordering system, global mutexes
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

The codebase has a lock ordering validation system (`LockOrder` enum in `gc/sync.rs`) designed to prevent deadlocks by enforcing a global lock acquisition order:
- Level 1: `LocalHeap`, `SegmentManager`
- Level 2: `GlobalMarkState`
- Level 3: `GcRequest`

However, several global mutexes are NOT protected by this validation system:

### Affected Global Mutexes:

1. **`CROSS_THREAD_SATB_BUFFER`** (`heap.rs:32`)
   - Type: `parking_lot::Mutex<Vec<usize>>`
   - Used for: Cross-thread SATB buffer
   - Not in lock ordering system

2. **`GcRootSet`** (`tokio/root.rs:25`)
   - Type: `std::sync::Mutex<Vec<usize>>`
   - Used for: Process-level GC root tracking
   - Not in lock ordering system

3. **`SEGMENT_MANAGER`** (`heap.rs:1491`)
   - Type: `std::sync::Mutex<GlobalSegmentManager>`
   - Used for: Global memory management
   - Has `LockOrder::SegmentManager` constant defined but NOT USED

4. **`THREAD_REGISTRY`** (`heap.rs:614`)
   - Type: `std::sync::Mutex<ThreadRegistry>`
   - Has debug-only validation treating it as level 2

### 預期行為 (Expected Behavior)

All global mutexes should either:
1. Be part of the lock ordering system with proper validation, OR
2. Be documented as allowed to be acquired in any order

### 實際行為 (Actual Behavior)

Several critical global mutexes bypass the lock ordering validation:
- `segment_manager()` returns a plain `Mutex` without lock ordering checks
- `CROSS_THREAD_SATB_BUFFER` is a plain `parking_lot::Mutex` 
- `GcRootSet` uses `std::sync::Mutex` without validation

This creates latent deadlock risk if future code changes introduce lock acquisition in different orders.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The lock ordering system (`gc/sync.rs:135-207`) defines `LockOrder` enum and validation functions, but:

1. **SEGMENT_MANAGER**: The constant `LockOrder::SegmentManager` exists (`gc/sync.rs:198`) but the actual `SEGMENT_MANAGER` global at `heap.rs:1491` is a plain `std::sync::Mutex` that never uses `acquire_lock()`.

2. **CROSS_THREAD_SATB_BUFFER**: Defined at `heap.rs:32` as `parking_lot::Mutex` without any lock ordering.

3. **GcRootSet**: Defined at `tokio/root.rs:25` as `std::sync::Mutex` without lock ordering.

The validation only happens in `debug_assertions` mode (see `heap.rs:629-633`), so in release builds there is NO enforcement at all.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Currently no observed deadlocks - this is a latent design issue. The bug would manifest if:
1. Code acquires one of these unprotected locks
2. Then tries to acquire a protected lock in wrong order

Example scenario:
```rust
// If future code does:
let _seg = segment_manager().lock().unwrap();  // Level 1 (but not validated)
let _thread = thread_registry().lock().unwrap(); // Level 2 (validated)
// This would be a reverse of correct order (thread then seg)
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **Option A - Integrate into LockOrder system**:
   - Add new variants to `LockOrder` enum for these mutexes
   - Update all acquisition sites to use `acquire_lock()` validation
   - Enable validation in release builds (or document why it must be debug-only)

2. **Option B - Document as exceptions**:
   - Add explicit documentation that these locks are exempt from ordering
   - Add runtime assertions in critical paths

3. **For SEGMENT_MANAGER specifically**:
   - The constant exists but isn't used - either integrate it properly or remove the constant

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The lock ordering is critical for GC correctness. During stop-the-world phases, multiple locks are acquired. If unprotected mutexes are acquired in different orders across different code paths, deadlocks become possible. The current code happens to acquire these in safe orders, but this is fragile.

**Rustacean (Soundness 觀點):**
The lock ordering validation is only enabled in debug builds. In release builds, there is NO protection against lock ordering violations. This is a significant gap - a subtle code change could introduce a deadlock that only manifests in production (release mode).

**Geohot (Exploit 觀點):**
This isn't directly exploitable for memory corruption, but denial-of-service via deadlock is possible. An attacker who can trigger specific GC paths could potentially cause the system to hang by introducing the right lock acquisition order.

