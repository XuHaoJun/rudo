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

## Cursor Integration
This project uses `.cursor/commands/` for custom speckit workflows:
- `speckit.specify.md` - Define requirements
- `speckit.plan.md` - Plan implementation
- `speckit.implement.md` - Execute implementation
- `speckit.analyze.md` - Analyze codebase

## Before Committing
1. Run `./clippy.sh` and fix all warnings
2. Run `./test.sh` and ensure all tests pass
3. Run `cargo fmt --all` to format code
4. For unsafe code changes, consider running `./miri-test.sh`

## Active Technologies
- Rust 1.75+ + `std::sync::atomic` (Rust stdlib), no external crates (002-send-sync-trait)
- N/A (in-memory garbage collector, heap managed internally) (002-send-sync-trait)
- Rust 1.75+ (stable, with `std::sync::atomic` features) + `std::sync::atomic`, `std::thread`, `std::sync::Barrier`, `std::sync::Mutex` (003-parallel-marking)
- Rust 1.75+ (as specified in AGENTS.md) + `std::sync::atomic`, `std::sync::Mutex`, `std::thread`, `std::sync::Barrier` (Rust stdlib only) (001-chez-gc-optimization)
- In-memory heap (N/A for external storage) (001-chez-gc-optimization)
- Rust 1.75+ (stable, with `std::sync::atomic` features) + tokio crate version 1.0+ (optional), tokio-util crate version 0.7+, rudo-gc-derive crate (004-tokio-async-integration)

## Recent Changes
- 002-send-sync-trait: Added Rust 1.75+ + `std::sync::atomic` (Rust stdlib), no external crates
- 004-tokio-async-integration: Added tokio async/await support with GcRootSet, GcRootGuard, #[gc::main], and Gc::yield_now()

## Tokio Async Integration (004-tokio-async-integration)

### Root Tracking
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

### Cooperative GC Scheduling
Use `Gc::yield_now()` to allow GC to run during long computations:

```rust
async fn process_large_dataset() {
    let gc = Gc::new(LargeDataSet::new());

    for item in dataset.iter() {
        // Process item
        gc.yield_now().await; // Allow GC to run
    }
}
```

## Concurrency Patterns (001-chez-gc-optimization)

### Lock Ordering Discipline
All locks must be acquired in a fixed global order to prevent deadlocks:
1. `LocalHeap` lock (per-thread heap)
2. `GlobalMarkState` lock (global marking state)
3. `GcRequest` lock (GC request)

Example:
```rust
let _heap_guard = self.heap.lock();
let _state_guard = GlobalMarkState::get().lock();
// Cannot acquire heap lock after state lock - violates order
```

### Push-Based Work Transfer
Workers push completed work to owner's queue instead of all workers polling:
- Each worker has `pending_work: Vec<GcPtr<T>>` (capacity 16)
- On buffer full, worker pushes to owner's `PerThreadMarkQueue::remote_work`
- Remote work is checked during steal attempts

### Mark Bitmap
Per-page mark bitmap replaces per-object overhead:
- `PageHeader::mark_bitmap: [AtomicU64; BITMAP_SIZE]`
- Each bit marks one object in the page
- 98% reduction in per-object overhead

### Segment Ownership
Track page ownership for better cache locality:
- `OwnedPagesTracker` maps page range to owner thread ID
- Workers mark pages they allocate into
- Helps prioritize marking local pages first

### Dynamic Stack Growth
Monitor work queue capacity to prevent stalls:
- Track `local_capacity` and `remote_capacity`
- Grow worklist when capacity threshold is reached
