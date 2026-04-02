# [Bug]: GcRwLockWriteGuard/GcMutexGuard/GcThreadSafeRefMut Drop Inconsistent with Write/Lock

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | When generational barrier active without incremental |
| **Severity (嚴重程度)** | High | May cause incorrect GC collection |
| **Reproducibility (復現難度)** | Medium | Requires specific barrier configuration |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLockWriteGuard`, `GcMutexGuard`, `GcThreadSafeRefMut` (sync.rs, cell.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current main branch

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`write()` and `lock()` call `mark_gc_ptrs_immediate(&*guard, true)` which marks GC pointers when ANY barrier is active. The corresponding `drop()` should do the same to maintain consistency.

### 實際行為 (Actual Behavior)
`drop()` only marks GC pointers when `incremental_active` is true, ignoring `generational_active`. This creates an inconsistency:

- `write()` / `lock()`: marks when `generational_active || incremental_active`
- `drop()`: marks only when `incremental_active`

When `generational_active = true` and `incremental_active = false`:
- `write()` marks pointers (due to bug479 fix passing `true`)
- `drop()` does NOT mark pointers

---

## 🔬 根本原因分析 (Root Cause Analysis)

### sync.rs - GcRwLockWriteGuard::drop() (line 484)
```rust
if incremental_active {  // BUG: Should be generational_active || incremental_active
    for gc_ptr in &ptrs {
        let _ = unsafe {
            crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
        };
    }
}
```

### sync.rs - GcMutexGuard::drop() (line 762)
```rust
if incremental_active {  // BUG: Should be generational_active || incremental_active
```

### cell.rs - GcThreadSafeRefMut::drop() (line 1491)
```rust
if incremental_active {  // BUG: Should be generational_active || incremental_active
```

Meanwhile `write()` (sync.rs:300) and `lock()` (sync.rs:612) call:
```rust
mark_gc_ptrs_immediate(&*guard, true);  // Always marks when any barrier active
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change all three locations from:
```rust
if incremental_active {
```

to:
```rust
if generational_active || incremental_active {
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The SATB barrier consistency is critical. When OLD values are recorded at write/lock time, NEW values must also be marked at drop time to maintain SATB invariants. The bug479 fix correctly marks at write/lock; the drop should match.

**Rustacean (Soundness 觀點):**
Inconsistent barrier behavior can lead to objects being incorrectly collected. This is a memory safety issue if a marked object is later freed.

**Geohot (Exploit 觀點):**
Use-after-free possible if generational barrier is active without incremental and the incorrectly unmarked object is collected.