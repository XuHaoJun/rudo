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

# Run tests with output
cargo test -- --nocapture --test-threads=1

# Run Miri for testing unsafe code
./miri-test.sh
```

**Note**: All test commands use `--test-threads=1` to avoid GC interference between parallel test threads. When tests run in parallel, one test's GC can interfere with another test's heap-allocated data structures (like `Vec<Gc<T>>`) that aren't directly traced from stack roots. This is a fundamental limitation of conservative GC.

## Code Style Guidelines

### Imports
- Use standard `use` declarations with module paths
- Group imports in this order:
  1. std library
  2. external crates
  3. internal modules (from same crate)
- Prefer absolute paths over `crate::` when possible for clarity
- Use `use std::...` for std library items, not `use crate::std::...`

Example:
```rust
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

use crate::heap::LocalHeap;
use crate::ptr::GcBox;
```

### Formatting
- Max line width: 100 characters (configured in rustfmt.toml)
- Tab spaces: 4
- Always run `cargo fmt` before committing
- Format with `cargo fmt --all` to format the entire workspace

### Types
- Prefer `T: ?Sized` for generic bounds when possible
- Use `NonNull<T>` for potentially-null raw pointers instead of `*mut T`
- Use `Cell<T>` for interior mutability in single-threaded contexts
- Use `AtomicUsize`, `AtomicBool`, etc. for thread-safe atomic operations

### Naming Conventions
- **Types/Structs/Enums**: `PascalCase` (e.g., `ThreadControlBlock`, `GcVisitor`)
- **Functions/Methods**: `snake_case` (e.g., `heap_mut`, `total_allocated`)
- **Constants**: `SCREAMING_SNAKE_CASE` (e.g., `THREAD_STATE_EXECUTING`)
- **Private fields**: `snake_case`
- **Acronyms**: Treat as words (e.g., `Bibop` not `BIBOP`, `Gc` not `GC`)
- **Test functions**: `test_<description>` (e.g., `test_basic_allocation`)

### Documentation
- All public items must have documentation (`#![warn(missing_docs)]`)
- Use module-level `//!` comments for module descriptions
- Use `///` for item-level documentation
- Include examples in documentation for important public APIs
- Use `#[must_use]` for functions where ignoring the return value is a bug

Example:
```rust
//! Mark-Sweep garbage collection algorithm.

/// Information about an object pending deallocation.
struct PendingDrop { ... }

/// Get a mutable reference to the heap.
#[must_use]
pub fn heap_mut(&mut self) -> &mut LocalHeap { ... }
```

### Error Handling
- Use `Result<T, E>` for recoverable errors
- Use `panic!` for unrecoverable errors (rare, mostly in tests)
- Use `unwrap()` and `expect()` only in tests or when invariant guarantees exist
- Use `unsafe { ... }` with explicit `// SAFETY:` comments explaining invariants

Example:
```rust
// SAFETY: This is safe because we have exclusive access through `UnsafeCell`
// and the heap pointer is guaranteed to be valid by the allocation logic.
unsafe { &*heap.tcb.heap.get() }.total_allocated()
```

### Lints and Warnings
- Workspace-level lints in `Cargo.toml`:
  - `unsafe_op_in_unsafe_fn = "warn"`: Warn on unsafe code in unsafe functions
  - Clippy `pedantic = "warn"`: Enable all pedantic lints
  - Clippy `nursery = "warn"`: Enable experimental lints
  - Clippy `cargo = "warn"`: Warn about cargo.toml issues
  - `multiple_crate_versions = "allow"`: Allow different versions of the same crate

### Testing Patterns
- Unit tests in `src/` files within `#[cfg(test)]` modules
- Integration tests in `tests/` directory
- Use `#[test]` for test functions
- Test files: `<feature>_test.rs` (e.g., `blacklisting_test.rs`)
- Use `#[derive(Trace)]` in tests with the `test-util` feature for test utilities
- Register test roots when needed: `register_test_root(ptr)` (for Miri tests)
- Clean up test roots: `clear_test_roots()`

Example:
```rust
#[test]
fn test_basic_allocation() {
    let x = Gc::new(42);
    assert_eq!(*x, 42);
}
```

### Unsafe Code Guidelines
- All unsafe code must have a `// SAFETY:` comment
- Explain the invariants being relied upon
- Document why the unsafe operation is safe
- Prefer safe abstractions over raw unsafe code when possible

### Module Organization
- Public modules: `pub mod name;`
- Private modules: `mod name;`
- Re-export important items at crate level for easy access
- Group related functionality into modules (e.g., `heap`, `trace`, `gc`, `ptr`)

### Feature Flags
- Default features: `derive` (for `#[derive(Trace)]` macro)
- `test-util`: Expose test utilities (`test_util` module)
- Use `#[cfg(feature = "derive")]` for optional derive macro re-export

### Workspace Structure
- Three crates: `rudo-gc` (main), `rudo-gc-derive` (proc macro), `sys_alloc` (system allocator)
- Use workspace dependencies for shared versioning
- Prefer `workspace = true` in member crate Cargo.toml files

### Performance Guidelines
- Use `#[inline]` for small, frequently-called functions
- Avoid unnecessary allocations in hot paths
- Use thread-local storage (`thread_local!`) for GC state
- Optimize for the "generational hypothesis": most objects die young

### Safety Invariants
- The project implements a garbage collector, so safety is critical
- Never assume Rust's borrowing rules for GC-managed objects
- The `Trace` trait MUST correctly report all `Gc<T>` fields
- Failure to trace all GC fields will cause use-after-free or memory leaks

### Cursor/Copilot Integration
- This project uses `.cursor/commands/` for custom spec workflows (speckit)
- No `.cursorrules` file exists
- No `.github/copilot-instructions.md` file exists

## Before Committing
1. Run `./clippy.sh` and fix all warnings
2. Run `./test.sh` and ensure all tests pass
3. Run `cargo fmt --all` to format code
4. For changes to unsafe code, consider running `./miri-test.sh`

## Common Tasks

### Adding a new GC-managed type
```rust
#[derive(Trace)]
struct MyStruct {
    value: i32,
    gc_ref: Gc<OtherType>,
}
```

### Running a single test
```bash
cargo test test_basic_allocation
```

### Adding a size class
Modify the heap module's size class configuration and ensure it integrates with BiBOP layout.
