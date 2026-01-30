# API Contracts: Tokio Async/Await Integration

**Feature**: 004-tokio-async-integration
**Date**: 2026-01-30

## Public API Surfaces

This directory contains API contract specifications for the tokio async/await integration.

## Files

| File | Description |
|------|-------------|
| `tokio-feature.md` | tokio feature flag behavior and conditional compilation |
| `macros.md` | #[gc::main] and #[gc::root] procedural macro contracts |
| `gc-spawn.md` | gc::spawn function API contract |

## Feature Gates

All tokio-specific features are gated behind the `tokio` feature flag:

```toml
[dependencies]
rudo-gc = { version = "0.1", features = ["tokio"] }
```

When the tokio feature is disabled:
- `GcTokioExt` trait is not available
- `gc::spawn()` function is not available
- `#[gc::main]` and `#[gc::root]` macros are not available
- The library compiles without tokio dependencies

## Version Requirements

| Dependency | Minimum Version | Required Features |
|------------|-----------------|-------------------|
| tokio | 1.0 | rt |
| tokio-util | 0.7 | rt |

## Error Handling Philosophy

All APIs use panic for programmer errors (unreachable code paths). No `Result` types are used because recoverable errors are not expected in this domain:

| Function | Error Behavior |
|----------|----------------|
| `Gc::root_guard()` | #[must_use]; panics if already guarded (debug) |
| `Gc::yield_now()` | Panics if not in tokio runtime context |
| `gc::spawn()` | Panics if tokio feature disabled |
| `#[gc::main]` | Compile error if not applied to async fn |
| `#[gc::root]` | Compile error if not applied to async block |
