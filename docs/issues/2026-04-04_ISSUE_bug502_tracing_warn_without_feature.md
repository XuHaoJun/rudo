# [Bug]: clear_overflow_queue uses tracing::warn! without tracing feature flag

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 100% reproducible - any build without `tracing` feature fails |
| **Severity (嚴重程度)** | High | Compilation error blocks all builds without tracing feature |
| **Reproducibility (復現難度)** | Very Low | Always fails when building without tracing feature |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/marker.rs::clear_overflow_queue` (line 193)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`clear_overflow_queue()` should only use `tracing::warn!` when the `tracing` feature is enabled, consistent with how the `tracing` module is conditionally compiled.

### 實際行為 (Actual Behavior)
`tracing::warn!` is called unconditionally at line 193, but the `tracing` module is only compiled when the `tracing` feature is enabled. This causes a compilation error when building without the `tracing` feature:

```
error[E0433]: cannot find module or crate `tracing` in this scope
   --> crates/rudo-gc/src/gc/marker.rs:193:13
    |
193 |             tracing::warn!(
    |             ^^^^^^^ use of unresolved module or unlinked crate `tracing`
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Cargo.toml:**
```toml
[features]
default = ["lazy-sweep", "derive"]
tracing = ["dep:tracing"]

[dependencies]
tracing = { version = "0.1", optional = true }
```

**lib.rs:**
```rust
#[cfg(feature = "tracing")]
mod tracing;
```

**marker.rs:185-200:**
```rust
let timeout = std::time::Duration::from_secs(5);
let start = std::time::Instant::now();
loop {
    let users = OVERFLOW_QUEUE_USERS.load(Ordering::Acquire);
    if users == 0 {
        break;
    }
    if start.elapsed() > timeout {
        tracing::warn!(  // <-- BUG: Not guarded by #[cfg(feature = "tracing")]
            "clear_overflow_queue: timeout waiting for {} users, proceeding anyway",
            users
        );
        break;
    }
    std::hint::spin_loop();
}
```

The bug501 fix introduced `tracing::warn!` without the proper `#[cfg(feature = "tracing")]` guard.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Wrap the `tracing::warn!` call with `#[cfg(feature = "tracing")]`:

```rust
if start.elapsed() > timeout {
    #[cfg(feature = "tracing")]
    tracing::warn!(
        "clear_overflow_queue: timeout waiting for {} users, proceeding anyway",
        users
    );
    break;
}
```

Or alternatively, use a compile-time conditional:

```rust
if start.elapsed() > timeout {
    log_timeout_warning(users);
    break;
}

#[cfg(feature = "tracing")]
fn log_timeout_warning(users: usize) {
    tracing::warn!(
        "clear_overflow_queue: timeout waiting for {} users, proceeding anyway",
        users
    );
}

#[cfg(not(feature = "tracing"))]
fn log_timeout_warning(_users: usize) {
    // No-op when tracing is disabled
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The tracing infrastructure was added for observability but the conditional compilation was not properly applied to all usage sites. This is a simple oversight in the bug501 fix.

**Rustacean (Soundness 觀點):**
This is a compilation error, not a runtime soundness issue. However, it prevents building without the tracing feature, which may be desired for production builds.

**Geohot (Exploit 觀點):**
No security implications - this is a build-time issue only.

---

## 驗證記錄

**驗證日期:** 2026-04-04
**驗證人員:** opencode

### 驗證結果

Build fails without `tracing` feature:
```
cargo build --workspace
error[E0433]: cannot find module or crate `tracing` in this scope
```

Build succeeds with `tracing` feature:
```
cargo build --workspace --features tracing
Finished `dev` profile
```

**Status: Open** - Needs fix.