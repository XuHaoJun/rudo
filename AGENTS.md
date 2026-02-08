# AGENTS.md

This document provides guidelines for agentic coding assistants working on the rudo-gc project.

## Build, Lint, and Test Commands

### Build
```bash
cargo build --workspace
cargo build --release --workspace
```

### Linting
```bash
# Run Clippy (treats warnings as errors)
./clippy.sh
# Or: cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format code
cargo fmt --all
```

### Testing
```bash
# Run all tests (lib, bins, integration, including ignored)
./test.sh
# Or: cargo test --lib --bins --tests --all-features -- --include-ignored --test-threads=1

# Run a single test
cargo test test_name -- --test-threads=1

# Run tests in a specific file
cargo test --test basic -- --test-threads=1

# Run Miri for testing unsafe code
./miri-test.sh
```

**Note**: All test commands use `--test-threads=1` to avoid GC interference between parallel test threads.

## Code Style Guidelines

### Imports
Group imports in this order:
1. std library
2. external crates
3. internal modules (from same crate)

Example:
```rust
use std::cell::UnsafeCell;
use std::collections::HashMap;
use crate::heap::LocalHeap;
use crate::ptr::GcBox;
```

### Formatting
- Max line width: 100 characters
- Tab spaces: 4
- Always run `cargo fmt --all` before committing

### Types
- Prefer `T: ?Sized` for generic bounds
- Use `NonNull<T>` for potentially-null raw pointers
- Use `Cell<T>` for single-threaded interior mutability

### Naming Conventions
- **Types/Structs/Enums**: `PascalCase`
- **Functions/Methods**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`
- **Acronyms**: Treat as words (e.g., `Bibop`, `Gc` not `GC`)
- **Test functions**: `test_<description>`

### Error Handling
- Use `Result<T, E>` for recoverable errors
- Use `panic!` for unrecoverable errors (mostly in tests)
- All unsafe code must have `// SAFETY:` comments

### Lints (Cargo.toml)
- `unsafe_op_in_unsafe_fn = "warn"`
- Clippy `pedantic` and `nursery` lints enabled

### Testing Patterns
- Unit tests in `#[cfg(test)]` modules
- Integration tests in `tests/` directory
- Use `#[derive(Trace)]` with `test-util` feature for test utilities
- Register test roots: `register_test_root(ptr)` for Miri tests

## Workspace Structure
- Three crates: `rudo-gc` (main), `rudo-gc-derive` (proc macro), `sys_alloc` (system allocator)
- Default features: `derive` for `#[derive(Trace)]` macro

## Agent Workflows
This project uses custom agentic workflows defined in `.agent/workflows/` (previously `.cursor/commands/`):
- `speckit.specify.md` - Define requirements and specifications
- `speckit.plan.md` - Plan implementation architecture
- `speckit.tasks.md` - Break down implementation into actionable tasks
- `speckit.implement.md` - Execute implementation plan
- `speckit.analyze.md` - Analyze codebase and artifacts for consistency
- `speckit.checklist.md` - Generate checklists for features
- `speckit.constitution.md` - Project constitution and principles

## Before Committing
1. Run `./clippy.sh` and fix all warnings
2. Run `./test.sh` and ensure all tests pass
3. Run `cargo fmt --all` to format code
4. For unsafe code changes, consider running `./miri-test.sh`

## Active Technologies
- **Core Language**: Rust 1.75+ (stable)
- **Concurrency**: `std::sync::atomic`, `std::sync::Mutex`, `std::sync::Barrier`, `std::thread` (Standard Library only)
- **Garbage Collection**: In-memory mark-sweep GC (no external heap storage dependencies)
- **Async Support**: `tokio` (optional feature `tokio` for async integration)
- Rust 1.75+ (stable) + `tracing` crate 0.1 (optional), `tracing-subscriber` (dev dependency for testing) (009-gc-tracing)
- N/A (in-memory spans, no persistence) (009-gc-tracing)
- Rust 1.75+ (stable) + `parking_lot` crate, existing `Trace` trait, existing write barrier infrastructure, existing STW pause mechanism (011-concurrent-gc-primitives)
- N/A (in-memory GC heap) (011-concurrent-gc-primitives)

## Recent Features & Changes

### 008 - Incremental Marking (In Progress)
- **Goal**: Reduce major GC pause times by splitting marking into cooperative increments
- **Algorithm**: Hybrid SATB (Snapshot-At-The-Beginning) + Dijkstra insertion barrier
- **Write Barriers**: Fast-path optimized barriers in `GcCell::borrow_mut()` with per-thread remembered buffer
- **Fallback**: Graceful STW fallback when dirty pages exceed threshold or slice timeout
- **Key Types**: `IncrementalMarkState`, `MarkPhase` enum, `IncrementalConfig`, `MarkStats`
- **Public API**: `set_incremental_config()`, `get_incremental_config()`, `is_incremental_marking_active()`

### 005 - Lazy Sweep (In Progress)
- Implements lazy sweeping to reduce pause times.
- Two-phase sweep: fast initial sweep for availability, background/lazy sweep for reclamation.

### 004 - Tokio Async Integration
- `GcRootSet` for tracking GC roots across async boundaries and tasks.
- `GcTokioExt` trait adds `yield_now()` for cooperative GC scheduling.
- `#[gc::main]` macro for async main function setup.

### 003 - Parallel Marking
- Concurrent marking using multiple worker threads.
- `GlobalMarkState` and `WorkStealingQueue` for load balancing.

### 002 - Send/Sync Trait Implementation
- Ensuring GC pointers and structures are correctly `Send` and `Sync` where appropriate.
- Usage of `Atomic` types for thread-safe internal state.

### 001 - Optimized Mark-Sweep (Chez Scheme inspired)
- **Lock Ordering**: Fixed global order (Heap -> GlobalMarkState -> Request) to prevent deadlocks.
- **Push-Based Work Transfer**: Workers push overflow work to owners.
- **Mark Bitmap**: Per-page bitmaps (`[AtomicU64; BITMAP_SIZE]`) reduce per-object overhead.
- **Segment Ownership**: Tracks page ownership for cache locality (`OwnedPagesTracker`).

## Major Feature Documentation

### Tokio Async Integration
The tokio integration uses a process-level singleton `GcRootSet` to track GC roots across async tasks:

```rust
use rudo_gc::tokio::{GcRootSet, GcRootGuard, GcTokioExt};

fn example() {
    let gc = Gc::new(Data { value: 42 });
    let _guard = gc.root_guard(); // Register as root

    tokio::spawn(async move {
        println!("{}", gc.value); // Safe to access
    });
}
```

**Cooperative GC Scheduling**: Use `Gc::yield_now()` to allow GC to run during long computations.

### Concurrency Patterns

**Lock Ordering Discipline**:
All locks must be acquired in a fixed global order:
1. `LocalHeap` lock (per-thread heap)
2. `GlobalMarkState` lock (global marking state)
3. `GcRequest` lock (GC request)

**Work Stealing & Balancing**:
- Workers have local work queues.
- On overflow, work is pushed to `PerThreadMarkQueue::remote_work`.
- Workers try to steal from remote queues when local is empty.

**Mark Bitmap & Page Layout**:
- Pages have headers with mark bitmaps.
- This separates metadata from object data, improving cache locality during marking.

## Recent Changes
- 011-concurrent-gc-primitives: Added Rust 1.75+ (stable) + `parking_lot` crate, existing `Trace` trait, existing write barrier infrastructure, existing STW pause mechanism
- 009-gc-tracing: Added Rust 1.75+ (stable) + `tracing` crate 0.1 (optional), `tracing-subscriber` (dev dependency for testing)
