# [Bug]: tokio_multi_runtime.rs test uses tokio features without cfg gate

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Very High | 100% reproducible - any build without `tokio` feature fails |
| **Severity (嚴重程度)** | Medium | Compilation error blocks testing without tokio feature |
| **Reproducibility (復現難度)** | Very Low | Always fails when running `cargo test` without `--features tokio` |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `tests/tokio_multi_runtime.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

Test functions that use `tokio` feature-gated functions should be wrapped with `#[cfg(feature = "tokio")]` to prevent compilation errors when the feature is not enabled.

### 實際行為 (Actual Behavior)

The test file `tokio_multi_runtime.rs` uses tokio runtime features and `GcRootSet`/`GcTokioExt` which require the `tokio` feature, but the test file itself is not gated:

```rust
// Line 9 - Uses tokio module
use rudo_gc::tokio::{GcRootSet, GcTokioExt};

// Line 24 - Uses tokio::runtime::Builder
let rt = tokio::runtime::Builder::new_multi_thread()

// Line 31 - Uses root_guard() method requiring tokio feature
let _guard = gc_clone.root_guard();
```

This causes compilation errors when running tests without the `tokio` feature:
```
error[E0433]: cannot find module or crate `tokio`
   --> crates/rudo-gc/tests/tokio_multi_runtime.rs:24:13
error[E0599]: no method named `root_guard` found for struct `Gc<i32>`
   --> crates/rudo-gc/tests/tokio_multi_runtime.rs:31:31
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

The tokio-specific APIs (`GcRootSet`, `GcTokioExt`, `root_guard`) are gated behind `#[cfg(feature = "tokio")]`. When building without this feature, these symbols don't exist. The test file includes this module unconditionally, causing compilation to fail.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```bash
# This fails with compilation error:
cargo test --lib --bins --tests -- --test-threads=1

# This works:
cargo test --lib --bins --tests --features tokio -- --test-threads=1
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `#[cfg(feature = "tokio")]` at the top of the test file:

```rust
#![allow(clippy::doc_markdown)]
#![allow(clippy::needless_pass_by_value)]

//! Integration test for multi-runtime support.
#![cfg(feature = "tokio")]
```

Or alternatively, gate individual test functions with `#[cfg(feature = "tokio")]`.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a test configuration issue similar to bug563 (test-util feature gate). Tests should compile and run regardless of feature flags unless they specifically require those features.

**Rustacean (Soundness 觀點):**
Not a soundness issue, but a build/test correctness issue. The test infrastructure should be consistent - if a test uses feature-gated functions, the test itself should be feature-gated.

**Geohot (Exploit 觀點):**
No security implications - this is a build-time issue only.