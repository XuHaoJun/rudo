# [Bug]: mark_object_minor CAS retry in Err branch without is_allocated check

**Status:** Open
**Tags:** Unverified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Requires CAS failure to occur simultaneously with slot deallocation |
| **Severity (嚴重程度)** | High | UB - reading generation from deallocated slot during retry |
| **Reproducibility (重現難度)** | Very Low | Requires Miri or ThreadSanitizer to detect |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `mark_object_minor` (gc/gc.rs:2143)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

When CAS fails in `try_mark` and the slot has been deallocated by lazy sweep, the loop should exit rather than retry on a deallocated slot.

### 實際行為 (Actual Behavior)

The `Err(())` branch in `mark_object_minor` (gc.rs:2143) does nothing and retries the loop without checking if the slot is still allocated:

```rust
Err(()) => {} // CAS failed, retry ← BUG: No is_allocated check!
```

If lazy sweep deallocates the slot between CAS failures, the retry reads from a deallocated slot.

---

## 根本原因分析 (Root Cause Analysis)

**問題位置：** `gc/gc.rs:2143`

```rust
loop {
    match (*header.as_ptr()).try_mark(index) {
        Ok(false) => {
            if !(*header.as_ptr()).is_allocated(index) {
                return;
            }
            return;
        }
        Ok(true) => {
            // Has is_allocated checks before reading generation
            // ...
        }
        Err(()) => {} // BUG: No is_allocated check!
    }
}
```

**對比 `mark_object` (gc.rs:2425-2454):**

`mark_object` checks `is_allocated` **before** the loop, so it's safe:

```rust
if !(*h).is_allocated(idx) {
    return None;
}

loop {
    match (*h).try_mark(idx) {
        // Err case is safe because is_allocated was checked before loop
        Err(()) => {}
    }
}
```

But `mark_object_minor` checks `is_allocated` **inside** the loop at lines 2107, 2115, 2123 - but NOT in the `Err` case.

---

## 重現步驟 / 概念驗證 (PoC)

Requires ThreadSanitizer or Miri to detect the data race between CAS failure and slot deallocation.

---

## 建議修復方案 (Suggested Fix)

Add `is_allocated` check in `Err` branch, matching the pattern in `Ok(false)` branch:

```rust
Err(()) => {
    if !(*header.as_ptr()).is_allocated(index) {
        break;
    }
    // Retry
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The CAS failure could occur after lazy sweep deallocates the slot. The retry should verify the slot is still allocated before continuing.

**Rustacean (Soundness 觀點):**
Reading from a deallocated slot is undefined behavior. The `is_allocated` check must be performed before any retry.

**Geohot (Exploit 觀點):**
While the race window is small, an attacker who could influence GC timing might trigger UB by causing the CAS to fail exactly when lazy sweep deallocates the slot.