# API Contract: GC Tracing Observability

**Feature**: 009-gc-tracing  
**Date**: 2026-02-05  

## Public API Surface

The tracing feature exposes minimal public API - only types needed for user correlation.

### Re-exported Types

#### `GcId`

```rust
#[cfg(feature = "tracing")]
pub use crate::tracing::internal::GcId;
```

**Purpose**: Allow users to correlate their own events with GC events.

**Example**:
```rust
use rudo_gc::GcId;

// Access current GC id during collection
// (exposed through metrics or callback APIs in future)
```

---

## Feature Flag

**Cargo.toml**:
```toml
[dependencies]
rudo-gc = { features = ["tracing"] }
```

**Conditional Compilation**:
```rust
#[cfg(feature = "tracing")]
// Tracing code here
```

---

## Span Structure (Internal)

Users interact with spans through `tracing` subscriber infrastructure, not directly.

### Span Names

| Span Name | Description | Fields |
|-----------|-------------|--------|
| `gc_collect` | Top-level collection span | `collection_type`, `gc_id` |
| `gc_phase` | Individual phase span | `phase` |
| `incremental_mark` | Incremental marking context | `phase` |

### Event Names

| Event Name | Level | Description |
|------------|-------|-------------|
| `phase_start` | DEBUG | Phase beginning |
| `phase_end` | DEBUG | Phase completion |
| `incremental_slice` | DEBUG | Mark slice completed |
| `fallback` | DEBUG | Fallback to STW |

---

## Integration Contract

### User Responsibilities

1. **Enable Feature**: Add `tracing` to Cargo.toml features
2. **Configure Subscriber**: Set up `tracing-subscriber` in application
3. **Filter Appropriately**: Use `rudo_gc=debug` for GC events only

### Library Guarantees

1. **Zero Cost**: No overhead when feature disabled
2. **DEBUG Level**: All spans/events use DEBUG level
3. **Structured**: Consistent field naming across events
4. **Correlation**: GcId links all events in a collection

---

## Usage Examples

### Basic Setup

```rust
use tracing_subscriber::{fmt, EnvFilter};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("rudo_gc=debug"))
        .init();
    
    // GC events will now appear in logs
}
```

### With Custom Subscriber

```rust
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::new("debug"))
        .init();
}
```

---

## Version Compatibility

- `tracing`: 0.1.x (stable, widely used)
- `tracing-subscriber`: 0.3.x (compatible with tracing 0.1)

Both are optional dependencies - only compiled when `tracing` feature enabled.
