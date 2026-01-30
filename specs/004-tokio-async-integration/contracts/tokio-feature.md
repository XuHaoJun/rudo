# Toko Feature Flag Contract

**Contract**: Feature flag behavior for tokio integration
**Date**: 2026-01-30

## Feature Declaration

```toml
# crates/rudo-gc/Cargo.toml

[features]
default = ["derive"]
derive = ["dep:rudo-gc-derive"]
tokio = ["dep:tokio", "dep:tokio-util"]

[dependencies]
tokio = { version = "1.0", optional = true, default-features = false, features = ["rt"] }
tokio-util = { version = "0.7", optional = true, features = ["rt"] }
```

## Conditional Compilation

### Available When `tokio` Feature Enabled

```rust
// crates/rudo-gc/src/tokio/mod.rs

#[cfg(feature = "tokio")]
pub trait GcTokioExt: Trace + Send + Sync {
    fn root_guard(&self) -> GcRootGuard;
    async fn yield_now(&self);
}

#[cfg(feature = "tokio")]
impl<T: Trace + Send + Sync> GcTokioExt for Gc<T> {
    fn root_guard(&self) -> GcRootGuard {
        let ptr = Gc::<T>::internal_ptr(self);
        GcRootGuard::new(unsafe {
            std::ptr::NonNull::new_unchecked(ptr as *mut u8)
        })
    }

    async fn yield_now(&self) {
        ::tokio::task::yield_now().await;
    }
}
```

### Not Available When `tokio` Feature Disabled

When the tokio feature is disabled:
- The `tokio` module is not exported
- `GcTokioExt` trait does not exist
- All `#[cfg(feature = "tokio")]` code is removed at compile time

## Default Behavior

- `default = ["derive"]` means derive feature is enabled by default
- `tokio` feature is **NOT** enabled by default
- Users must explicitly opt-in to tokio support

## Migration Path

### Opting In

```toml
# Cargo.toml
[dependencies]
rudo-gc = { version = "0.1", default-features = false, features = ["derive", "tokio"] }
```

### Opting Out (Default)

```toml
# Cargo.toml
[dependencies]
rudo-gc = { version = "0.1", features = ["derive"] }
```

## Dependency Behavior

| Scenario | tokio Dependency | tokio-util Dependency |
|----------|------------------|----------------------|
| tokio feature enabled | Required (optional = true) | Required (optional = true) |
| tokio feature disabled | Not included | Not included |
| Default features only | Not included | Not included |

## Build Impact

### With tokio Feature

- Full tokio integration available
- All tokio tests run
- Additional compile time for proc-macros

### Without tokio Feature

- Minimal dependencies
- No tokio-related code compiled
- Faster compile times
