# [Bug]: test_cross_thread_satb_dual_overflow_no_entry_loss uses test-util functions without cfg gate

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Very High | 100% reproducible - test fails to compile without test-util feature |
| **Severity (嚴重程度)** | Medium | Compilation error blocks testing without test-util feature |
| **Reproducibility (Reproducibility)** | Very Low | Always fails when running `cargo test` without `--all-features` |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc_thread_safe_cell.rs::test_cross_thread_satb_dual_overflow_no_entry_loss`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
Test functions that use `test-util` feature-gated functions should be wrapped with `#[cfg(feature = "test-util")]` to prevent compilation errors when the feature is not enabled.

### 實際行為 (Actual Behavior)
The test `test_cross_thread_satb_dual_overflow_no_entry_loss` uses `set_cross_thread_satb_capacity` and `flush_cross_thread_satb` which require the `test-util` feature, but the test function itself is not gated:

```rust
// Line 448 - Missing #[cfg(feature = "test-util")]
#[test]
fn test_cross_thread_satb_dual_overflow_no_entry_loss() {
    // Line 457 - Requires test-util feature
    rudo_gc::test_util::set_cross_thread_satb_capacity(1);
    // ...
    // Line 497 - Requires test-util feature
    let flushed = rudo_gc::test_util::flush_cross_thread_satb();
}
```

This causes a compilation error when running tests without the `test-util` feature:
```
error[E0425]: cannot find function `set_cross_thread_satb_capacity` in module `rudo_gc::test_util`
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `set_cross_thread_satb_capacity` and `flush_cross_thread_satb` functions are gated behind `#[cfg(any(test, feature = "test-util"))]` in `lib.rs`:

```rust
// lib.rs:264-268
#[cfg(any(test, feature = "test-util"))]
pub fn set_cross_thread_satb_capacity(cap: usize) { ... }

// lib.rs:276-279
#[cfg(any(test, feature = "test-util"))]
pub fn flush_cross_thread_satb() -> usize { ... }
```

When running `cargo test --lib --bins --tests -- --test-threads=1` without `--all-features`, the `test-util` feature is not enabled, so these functions don't exist. However, the test function is not gated, causing a compilation error.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```bash
# This fails with compilation error:
cargo test --lib --bins --tests -- --test-threads=1

# This works:
cargo test --lib --bins --tests --all-features -- --test-threads=1
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `#[cfg(feature = "test-util")]` to the test function:

```rust
#[cfg(feature = "test-util")]
#[test]
fn test_cross_thread_satb_dual_overflow_no_entry_loss() {
    // ... existing test code ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a test configuration issue that prevents proper test coverage verification. Tests should compile and run regardless of feature flags unless they specifically require those features.

**Rustacean (Soundness 觀點):**
Not a soundness issue, but a build/test correctness issue. The test infrastructure should be consistent - if a test uses feature-gated functions, the test itself should be feature-gated.

**Geohot (Exploit 觀點):**
No security implications - this is a build-time issue only.

---

## 驗證記錄

**驗證日期:** 2026-04-10
**驗證人員:** opencode

### 驗證結果

Confirmed: Running `cargo test --lib --bins --tests -- --test-threads=1` produces:
```
error[E0425]: cannot find function `set_cross_thread_satb_capacity` in module `rudo_gc::test_util`
   --> crates/rudo-gc/tests/gc_thread_safe_cell.rs:457:25
```

The same error occurs for `flush_cross_thread_satb` at line 497.

**Status: Open** - Needs fix.

---

## Resolution (2026-04-10)

**Applied fix:** Added `#[cfg(feature = "test-util")]` to the test function.

Code change in `crates/rudo-gc/tests/gc_thread_safe_cell.rs`:
- Line 448: Added `#[cfg(feature = "test-util")]` before `#[test]`

Verification: `cargo test --lib --bins --tests -- --test-threads=1` now compiles and runs successfully.
