# Quickstart: GC Tracing

Enable structured logging for garbage collection operations in rudo-gc.

## Installation

Add the `tracing` feature to your `Cargo.toml`:

```toml
[dependencies]
rudo-gc = { features = ["tracing"] }
tracing-subscriber = "0.3"  # For configuring output
```

## Basic Setup

Configure a tracing subscriber in your application entry point:

```rust
use tracing_subscriber::{fmt, EnvFilter};

fn main() {
    // Initialize subscriber with filter for GC debug logs
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("rudo_gc=debug"))
        .init();
    
    // Your application code here
    // GC events will automatically appear in logs
}
```

## What You'll See

When tracing is enabled and a garbage collection runs:

```
2024-01-01T12:00:00.000Z DEBUG rudo_gc::gc: gc_collect collection_type="major_multi_threaded" gc_id=42
2024-01-01T12:00:00.001Z DEBUG rudo_gc::gc: gc_phase phase="clear"
2024-01-01T12:00:00.002Z DEBUG rudo_gc::gc: phase_start phase="clear" bytes_before=10485760
2024-01-01T12:00:00.003Z DEBUG rudo_gc::gc: phase_end phase="clear" bytes_reclaimed=0
2024-01-01T12:00:00.004Z DEBUG rudo_gc::gc: gc_phase phase="mark"
2024-01-01T12:00:00.005Z DEBUG rudo_gc::gc: phase_start phase="mark" bytes_before=10485760
2024-01-01T12:00:00.050Z DEBUG rudo_gc::gc: phase_end phase="mark" bytes_reclaimed=0 objects_marked=1234
2024-01-01T12:00:00.051Z DEBUG rudo_gc::gc: gc_phase phase="sweep"
2024-01-01T12:00:00.052Z DEBUG rudo_gc::gc: sweep_start heap_bytes=10485760
2024-01-01T12:00:00.100Z DEBUG rudo_gc::gc: sweep_end objects_freed=100 bytes_freed=524288
```

## Advanced Configuration

### JSON Output

```rust
use tracing_subscriber::fmt::format::FmtSpan;

tracing_subscriber::fmt()
    .json()
    .with_env_filter(EnvFilter::new("rudo_gc=debug"))
    .init();
```

### Filtering Specific Events

```rust
// Only show major collections
EnvFilter::new("rudo_gc=debug[gc_collect{collection_type=major*}]")

// Show everything including incremental marks
EnvFilter::new("rudo_gc=debug,rudo_gc::gc::incremental=trace")
```

### Custom Layer

```rust
use tracing_subscriber::layer::SubscriberExt;

let gc_layer = tracing_subscriber::fmt::layer()
    .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
        meta.target().starts_with("rudo_gc")
    }));

tracing_subscriber::registry()
    .with(gc_layer)
    .init();
```

## Performance Notes

- **Zero cost**: When `tracing` feature is disabled, no code is generated
- **Minimal overhead**: DEBUG level ensures events are cheap when filtered
- **No allocation**: All trace data uses stack-allocated values

## Troubleshooting

### No output appears

- Verify feature is enabled: `cargo build --features tracing`
- Check filter level: `rudo_gc=debug` minimum
- Ensure subscriber is initialized before GC runs

### Too much output

- Use more specific filters: `rudo_gc::gc=debug` (exclude incremental)
- Filter by collection type in your subscriber
- Use sampling for high-frequency collections

## Next Steps

- See [Feature Spec](spec.md) for detailed requirements
- See [Data Model](data-model.md) for trace structure
- See [API Contract](contracts/api-surface.md) for integration details
