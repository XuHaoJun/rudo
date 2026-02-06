# rudo-gc Tracing Feature Implementation Plan

## 1. Overview

### Goal
Add an optional `tracing` feature that allows users to observe garbage collection execution (similar to gc-arena's approach).

### Reference Implementation
gc-arena uses the `tracing` crate, implementing a `PhaseGuard` pattern in `context.rs`:
- Each GC phase creates a parent span
- Uses `#[cfg(feature = "tracing")]` for conditional compilation
- Uses `tracing::debug!()` to log progress

### Design Principles
- **Zero-cost**: When the `tracing` feature is not enabled, no extra code is generated
- **Low-cost**: When enabled, use `debug!` level to avoid `info!` recording overhead
- **Structured**: Clearly distinguish between GC phases
- **Context-safe**: Explicitly propagate spans across worker threads where needed

---

## 2. Add Dependencies

### `crates/rudo-gc/Cargo.toml`

```toml
[features]
default = ["lazy-sweep", "derive"]
derive = ["dep:rudo-gc-derive"]
lazy-sweep = []
test-util = []
tokio = ["dep:tokio", "dep:tokio-util", "dep:rudo-gc-tokio-derive"]
tracing = ["dep:tracing"]  # NEW

[dependencies]
# ...existing dependencies...
tracing = { version = "0.1", optional = true }
```

---

## 3. New Modules

### `src/tracing.rs` (New File)

Create tracing submodule:

```rust
//! GC tracing support.
//!
//! When the `tracing` feature is enabled, this module provides structured
//! tracing spans and events for garbage collection operations.

#[cfg(feature = "tracing")]
pub mod internal {
    use crate::metrics::CollectionType;
    use tracing::{span, Level};

    /// High-level GC phases (clear/mark/sweep).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum GcPhase {
        Clear,
        Mark,
        Sweep,
    }

    /// Stable identifier for a GC run.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GcId(pub u64);

    // Span level definitions
    pub fn trace_gc_collection(collection_type: CollectionType, gc_id: GcId) -> span::EnteredSpan {
        span!(
            Level::DEBUG,
            "gc_collection",
            collection_type = ?collection_type,
            gc_id = gc_id.0
        )
        .entered()
    }

    pub fn trace_phase(phase: GcPhase) -> span::EnteredSpan {
        span!(Level::DEBUG, "gc_phase", phase = ?phase).entered()
    }

    // Event logging
    pub fn log_phase_start(phase: GcPhase, bytes: usize) {
        tracing::debug!(phase = ?phase, bytes_before = bytes, "phase_start");
    }

    pub fn log_phase_end(phase: GcPhase, reclaimed: usize) {
        tracing::debug!(phase = ?phase, bytes_reclaimed = reclaimed, "phase_end");
    }
}
```

### `src/gc/tracing.rs` (New File)

GC-specific tracing:

```rust
//! GC-level tracing spans.

use crate::gc::incremental::MarkPhase;

#[cfg(feature = "tracing")]
pub fn span_incremental_mark(phase: MarkPhase) -> tracing::Span {
    tracing::debug_span!("incremental_mark", phase = ?phase)
}
```

---

## 4. Modify Existing Modules

### 4.1 `src/gc/incremental.rs`

Add tracing spans to incremental marking:

```rust
#[cfg(feature = "tracing")]
let _span = span_incremental_mark(state.phase());

// On phase transitions:
#[cfg(feature = "tracing")]
tracing::debug!(
    phase = ?new_phase,
    objects_marked = state.stats.objects_marked.load(Ordering::Relaxed),
    "phase_transition"
);
```

**Modification Locations**:
- `mark_slice()` - incremental marking slice
- `execute_final_mark()` - final mark phase
- `IncrementalMarkState::set_phase()` - phase transitions

### 4.2 `src/gc/gc.rs`

Add a monotonic GC id counter (internal, behind feature):

```rust
#[cfg(feature = "tracing")]
static NEXT_GC_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

#[cfg(feature = "tracing")]
fn next_gc_id() -> crate::tracing::internal::GcId {
    use std::sync::atomic::Ordering;
    crate::tracing::internal::GcId(NEXT_GC_ID.fetch_add(1, Ordering::Relaxed))
}
```

Add spans in collection functions:

**`perform_multi_threaded_collect()`**:
```rust
#[cfg(feature = "tracing")]
let _gc_span = tracing::debug_span!(
    "gc_collect",
    collection_type = "major_multi_threaded"
).entered();

// Phase 1: Clear
#[cfg(feature = "tracing")]
let _clear_span = tracing::debug_span!("gc_phase", phase = "clear").entered();

// Phase 2: Mark
#[cfg(feature = "tracing")]
let _mark_span = tracing::debug_span!("gc_phase", phase = "mark").entered();

// Phase 3: Sweep
#[cfg(feature = "tracing")]
let _sweep_span = tracing::debug_span!("gc_phase", phase = "sweep").entered();
```

**`perform_single_threaded_collect_full()`**:
- Same pattern, wrapped in single-threaded context

**`collect_minor()`**:
```rust
#[cfg(feature = "tracing")]
let _span = tracing::debug_span!("gc_collect", collection_type = "minor").entered();
```

### 4.3 `src/lib.rs`

Re-export tracing API:

```rust
#[cfg(feature = "tracing")]
pub use crate::tracing::internal::GcId;
```

---

## 5. New Traced Events

### 5.1 Collection Events

| Event | Fields | Trigger |
|-------|--------|---------|
| `gc_start` | `collection_type`, `heap_bytes`, `gc_id` | `collect()` entry |
| `gc_end` | `bytes_reclaimed`, `duration_ms` | `collect()` exit |
| `phase_start` | `phase`, `bytes` | Phase entry |
| `phase_end` | `phase`, `bytes_reclaimed` | Phase exit |

### 5.2 Incremental Mark Events

| Event | Fields | Trigger |
|-------|--------|---------|
| `incremental_start` | `budget`, `gc_id` | `incremental_mark_slice()` entry |
| `incremental_slice` | `objects_marked`, `dirty_pages` | Slice completed |
| `fallback` | `reason` | Fallback to STW |

### 5.3 Sweep Events

| Event | Fields | Trigger |
|-------|--------|---------|
| `sweep_start` | `heap_bytes` | Sweep entry |
| `object_freed` | (recorded in bulk) | Sweep progress |
| `sweep_end` | `objects_freed`, `bytes_freed` | Sweep exit |

---

## 6. Implementation Steps (Task Breakdown)

### Phase 1: Infrastructure
- [ ] Task 1.1: Update `Cargo.toml` to add `tracing` dependency
- [ ] Task 1.2: Create `src/tracing.rs` module
- [ ] Task 1.3: Create `src/gc/tracing.rs` module
- [ ] Task 1.4: Add `GcPhase` and `GcId` types (internal)
- [ ] Task 1.5: Wire modules (`mod tracing;` in `lib.rs`, `gc::mod.rs`)

### Phase 2: Collection Tracing
- [ ] Task 2.1: Add spans in `perform_multi_threaded_collect()`
- [ ] Task 2.2: Add spans in `perform_single_threaded_collect_full()`
- [ ] Task 2.3: Add spans in `collect_minor()`
- [ ] Task 2.4: Add spans in `collect_major()`, `collect_major_multi()`
- [ ] Task 2.5: Propagate parent span to worker threads (explicit `Span::enter`)

### Phase 3: Phase-Level Tracing
- [ ] Task 3.1: Add span/event in `clear_all_marks_and_dirty()`
- [ ] Task 3.2: Add spans in marking functions
- [ ] Task 3.3: Add spans in `sweep_phase1_finalize()`, `sweep_phase2_reclaim()`

### Phase 4: Incremental Mark Tracing
- [ ] Task 4.1: Add spans in `incremental_mark_slice()`
- [ ] Task 4.2: Add spans in `execute_final_mark()`
- [ ] Task 4.3: Add phase transition events in `set_phase()`

### Phase 5: Testing and Documentation
- [ ] Task 5.1: Add integration test to verify tracing output
- [ ] Task 5.2: Update `README.md` to document `tracing` feature
- [ ] Task 5.3: Run `cargo clippy --all-features` to confirm no warnings

---

## 7. Example Usage

### 7.1 Enable Tracing

```toml
# Cargo.toml
rudo-gc = { features = ["tracing"] }
```

### 7.2 Configure Subscriber

```rust
use tracing_subscriber::{fmt, EnvFilter};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("rudo_gc=debug"))
        .init();

    // ... your code ...
}
```

### 7.3 Expected Output

```
2024-01-01T12:00:00.000Z DEBUG rudo_gc::gc: gc_collect collection_type="major_multi_threaded"
2024-01-01T12:00:00.001Z DEBUG rudo_gc::gc: phase_start phase="clear" bytes_before=10485760
2024-01-01T12:00:00.002Z DEBUG rudo_gc::gc: phase_end phase="clear" bytes_reclaimed=0
2024-01-01T12:00:00.003Z DEBUG rudo_gc::gc: phase_start phase="mark" bytes_before=10485760
2024-01-01T12:00:00.050Z DEBUG rudo_gc::gc: phase_end phase="mark" bytes_reclaimed=0 objects_marked=1234
2024-01-01T12:00:00.051Z DEBUG rudo_gc::gc: phase_start phase="sweep" bytes_before=10485760
2024-01-01T12:00:00.100Z DEBUG rudo_gc::gc: phase_end phase="sweep" bytes_reclaimed=524288 objects_freed=100
```

---

## 8. Potential Issues

### 8.1 Multi-threading Scenarios
- Each thread's GC operation will have its own span
- Need to ensure spans don't interleave causing confusion
- Solution: explicitly propagate the parent span into worker threads
  (e.g., capture `Span` and `enter` it inside worker closures)

### 8.2 Performance Impact
- `#[cfg(feature = "tracing")]` ensures zero-cost when disabled
- When enabled, use `debug!` level for minimal overhead

### 8.3 Integration with Existing `eprintln!`
- Consider converting existing `eprintln!("[GC] Incremental marking fallback...")` to `tracing::warn!()`
- This is not necessary since `eprintln!` remains visible

---

## 9. Priority Order

| Priority | Task | Reason |
|----------|------|--------|
| P0 | Cargo.toml + Infrastructure | Dependency for other tasks |
| P1 | Collection-level tracing | Most common use case |
| P2 | Incremental marking tracing | Observability for advanced features |
| P3 | Phase-level tracing | Detailed debugging |
| P4 | Testing and documentation | Ensure correctness |

---

## Appendix: Comparison with gc-arena

| Feature | rudo-gc (New) | gc-arena (Reference) |
|---------|---------------|----------------------|
| Feature flag | `tracing` | `tracing` |
| Tracing crate | `tracing` 0.1.40 | `tracing` 0.1 |
| Span structure | Per-phase | Per-phase via PhaseGuard |
| Metrics integration | Existing `GcMetrics` | Detailed `Pacing` struct |
| Incremental support | Full tracing | Basic phase tracking |
| Target | `rudo_gc` | `gc_arena` |

---

## References

- [gc-arena Tracing Implementation](https://github.com/kyren/gc-arena)
- [tracing crate documentation](https://docs.rs/tracing)
- [tracing subscriber tutorial](https://docs.rs/tracing-subscriber)
