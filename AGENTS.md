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
