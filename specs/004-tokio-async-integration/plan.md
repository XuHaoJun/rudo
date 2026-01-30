# Implementation Plan: Tokio Async/Await Integration

**Branch**: `[004-tokio-async-integration]` | **Date**: 2026-01-30 | **Spec**: [link](../spec.md)
**Input**: Feature specification from `/specs/004-tokio-async-integration/spec.md`

## Summary

Integrate rudo-gc with tokio async/await system using drop guard pattern and proc-macro automation. Based on tokio-rs patterns from `tokio-macros/src/entry.rs` (runtime builder pattern), `tokio-util/src/task/task_tracker.rs` (atomic counting with Notify), and `tokio-util/src/task/spawn_pinned.rs` (oneshot channel + guard pattern).

## Technical Context

**Language/Version**: Rust 1.75+ (stable, with `std::sync::atomic` features)  
**Primary Dependencies**: tokio crate version 1.0+ (optional), tokio-util crate version 0.7+, rudo-gc-derive crate  
**Storage**: N/A (in-memory garbage collector, heap managed internally)  
**Testing**: cargo test, Miri tests for unsafe code, `--test-threads=1` for GC interference tests  
**Target Platform**: Linux, macOS, Windows (cross-platform via std::sync::atomic)  
**Project Type**: Rust library (rudo-gc crate + rudo-gc-derive proc-macro crate)  
**Performance Goals**: Root registration/unregistration < 1μs, 10,000 concurrent tasks supported, memory overhead < 32 bytes per root  
**Constraints**: Must pass Miri tests, no undefined behavior, drop guard pattern for RAII root management  
**Scale/Scope**: Single GC, multiple tokio runtimes; process-level root tracking

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Requirement | Status | Notes |
|-------------|--------|-------|
| I. Memory Safety (NON-NEGOTIABLE) | **PASS** | GcRootGuard uses RAII pattern; All unsafe code requires SAFETY comments |
| II. Testing Discipline (NON-NEGOTIABLE) | **PASS** | Integration tests required for tokio integration; Miri tests for unsafe pointer operations |
| III. Performance-First Design | **PASS** | Atomic operations for root tracking; BiBOP allocation unchanged |
| IV. API Consistency | **PASS** | Follows Rust conventions (snake_case functions, PascalCase types); GcTokioExt trait extension |
| V. Cross-Platform Reliability | **PASS** | Uses std::sync::atomic (cross-platform); No platform-specific code |

**Code Quality Gates**: All apply
- `./clippy.sh` passes with zero warnings
- `cargo fmt --all` produces no changes
- `./test.sh` passes all tests including ignored
- `./miri-test.sh` passes for unsafe code changes
- Public APIs have doc comments with examples

## Project Structure

### Documentation (this feature)

```text
specs/004-tokio-async-integration/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (tokio-rs patterns analysis)
├── data-model.md        # Phase 1 output (entity definitions)
├── quickstart.md        # Phase 1 output (API usage guide)
├── contracts/           # Phase 1 output (API specifications)
│   └── README.md
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created by /speckit.plan)
```

### Source Code (repository root)

```text
crates/rudo-gc/
├── Cargo.toml                          # [modify - tokio feature]
├── src/
│   ├── lib.rs                          # [modify - pub mod tokio]
│   └── tokio/
│       ├── mod.rs                      # [modify - GcTokioExt trait]
│       ├── root.rs                     # [new - GcRootSet]
│       ├── guard.rs                    # [new - GcRootGuard]
│       └── spawn.rs                    # [new - gc::spawn wrapper]
└── tests/
    └── tokio_integration.rs            # [new - integration tests]

crates/rudo-gc-derive/
├── Cargo.toml                          # [no changes]
└── src/
    ├── lib.rs                          # [modify - export main/root macros]
    ├── main.rs                         # [new - #[gc::main] macro]
    └── root.rs                         # [new - #[gc::root] macro]
```

**Structure Decision**: Adding new tokio/ module under rudo-gc/src/ for tokio-specific features. Adding new proc-macro source files under rudo-gc-derive/src/ for macro implementations.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| N/A | N/A | N/A |

---

# Phase 0: Outline & Research

## Research Findings: Tokio Async/Await Integration Patterns

### 1. Runtime Initialization Pattern (from tokio-macros/src/entry.rs)

**Decision**: Use runtime builder pattern for #[gc::main] macro

**Rationale**: The tokio::main macro demonstrates a robust pattern for runtime creation:
- Extract function signature and body using syn
- Build runtime using `Builder::new_multi_thread()` or `Builder::new_current_thread()`
- Wrap function body in `runtime.block_on(async { ... })`
- Support configurable options (worker_threads, flavor, unhandled_panic)

**Alternatives considered**:
- Direct tokio::spawn without runtime: Rejected - requires user to create runtime manually
- Runtime::current(): Rejected - doesn't guarantee GcRootSet initialization

**Key patterns to replicate**:
```rust
// Configuration parsing from attribute args
// Asyncness validation
// Runtime builder with enable_all()
// block_on wrapper for async function body
```

### 2. Task Tracking Pattern (from tokio-util/src/task/task_tracker.rs)

**Decision**: Use atomic counting pattern for root tracking

**Rationale**: TaskTracker uses `AtomicUsize` for efficient task counting:
- `fetch_add(2, Ordering::Relaxed)` for adding tasks
- `fetch_sub(2, Ordering::Release)` for removing tasks
- Closed state stored in lowest bit
- Notify for wait completion

**Alternatives considered**:
- Mutex-protected counter: Rejected - higher overhead than atomic operations
- Rc<AtomicUsize>: Rejected - process-level singleton needed for multi-runtime

**Key patterns to replicate**:
```rust
// Atomic counting with bit packing for state
// Ordering semantics (AcqRel for synchronization)
// Drop-based cleanup (Drop for TaskTrackerToken)
```

### 3. Spawn Pinning Pattern (from tokio-util/src/task/spawn_pinned.rs)

**Decision**: Use oneshot channel + guard pattern for gc::spawn wrapper

**Rationale**: spawn_pinned demonstrates safe ownership transfer:
- `oneshot::channel()` for sending JoinHandle back
- `JobCountGuard` for automatic cleanup on drop
- Async task wrapper that holds guard until completion

**Alternatives considered**:
- Direct tokio::spawn: Rejected - no automatic root tracking
- Arc<GcRootGuard>: Rejected - unnecessary reference counting overhead

**Key patterns to replicate**:
```rust
// Future wrapper that owns the guard
// Automatic unregistration on drop
// Abort handling for task cancellation
```

### 4. Dirty Flag Pattern

**Decision**: Use AtomicBool for root set dirty flag

**Rationale**: Enables GC to skip collection cycles when roots unchanged:
- Store dirty flag with atomic operations
- GC checks flag before collection
- Clear flag after snapshot

**Key patterns to replicate**:
```rust
// AtomicBool store/load with appropriate ordering
// Check-during-collection pattern
```

---

# Phase 1: Design & Contracts

## Data Model

### Entities

| Entity | Fields | Relationships | Notes |
|--------|--------|---------------|-------|
| GcRootSet | roots: Mutex<Vec<usize>>, count: AtomicUsize, dirty: AtomicBool | Singleton (OnceLock) | Process-level root tracking |
| GcRootGuard | ptr: usize, _phantom: PhantomData<u8> | Owned by user code | RAII root registration |
| GcRootScope<F> | future: F, _guard: GcRootGuard | Wraps future for spawn | Automatic root tracking |
| GcTokioExt | Trait methods: root_guard(), yield_now() | Implemented for Gc<T> | Extension trait when tokio enabled |

### Validation Rules

- GcRootSet::register() must reject duplicate pointers
- GcRootGuard must be #[must_use]
- GcRootSet must be process-level singleton (OnceLock)
- Dirty flag must be set on any root modification

### State Transitions

```
GcRootSet lifecycle:
  clean (dirty=false) --register/unregister--> dirty (dirty=true)
  dirty --GC snapshot + clear_dirty()--> clean

GcRootGuard lifecycle:
  new (registers root) --drop--> unregistered
```

## API Contracts

### Public API Surfaces

1. **GcTokioExt trait** (when tokio feature enabled):
   - `fn root_guard(&self) -> GcRootGuard`
   - `async fn yield_now(&self)`

2. **gc::spawn function**:
   - `pub async fn spawn<F, T>(future: F) -> JoinHandle<F::Output>`
   - where F: Future + Send + 'static, F::Output: Send + 'static

3. **#[gc::main] procedural macro**:
   - Attribute: `#[gc::main(flavor = "multi_thread", worker_threads = None)]`
   - Transforms async fn into blocking runtime wrapper

4. **#[gc::root] procedural macro**:
   - Attribute: `#[gc::root]`
   - Wraps async block with automatic GcRootGuard

### Error Handling

- All APIs use panic for programmer errors (unreachable code paths)
- No Result types (recoverable errors not expected in this domain)
- #[must_use] on guards to prevent accidental early drop

## Quickstart Guide Structure

```markdown
# Tokio Async/Await Quickstart

## Basic Usage
- Create Gc pointer
- Use root_guard() for manual tracking
- Spawn tokio tasks accessing Gc

## Proc-Macro Usage
- #[gc::main] for runtime initialization
- #[gc::root] for automatic root tracking
- gc::spawn for automatic spawn wrapping

## Examples
- Complete runnable examples for each pattern
- Integration test code demonstrating correctness
```

## Contracts Directory

```
contracts/
├── README.md                    # API surface documentation
├── tokio-feature.md             # tokio feature flag behavior
├── macros.md                    # #[gc::main] and #[gc::root] usage
└── gc-spawn.md                  # gc::spawn API contract
```

---

## Agent Context Update

```bash
.specify/scripts/bash/update-agent-context.sh opencode
```

**New technologies from this plan**:
- tokio runtime builder pattern
- AtomicUsize counting with bit packing
- OnceLock for process-level singleton
- Procedural macro attribute parsing (syn/quote)
- Future wrapper pattern (pin_project alternative using Pin)

**Preserved manual additions**: None (initial context)

---

## Re-check: Constitution Post-Design

| Requirement | Status | Notes |
|-------------|--------|-------|
| I. Memory Safety (NON-NEGOTIABLE) | **PASS** | RAII guards ensure cleanup; Miri tests required |
| II. Testing Discipline (NON-NEGOTIABLE) | **PASS** | Integration tests for tokio workflows; Miri for unsafe |
| III. Performance-First Design | **PASS** | Atomics for O(1) operations; minimal overhead |
| IV. API Consistency | **PASS** | snake_case functions, PascalCase types, doc comments |
| V. Cross-Platform Reliability | **PASS** | Uses std::sync::atomic only |

**Summary**: All constitution requirements continue to pass after detailed design. Implementation ready for Phase 2 (tasks generation).

---

## Artifacts Generated

| Artifact | Path |
|----------|------|
| Implementation Plan | `/home/noah/Desktop/rudo/specs/004-tokio-async-integration/plan.md` |
| Research Document | `/home/noah/Desktop/rudo/specs/004-tokio-async-integration/research.md` |
| Data Model | `/home/noah/Desktop/rudo/specs/004-tokio-async-integration/data-model.md` |
| Quickstart | `/home/noah/Desktop/rudo/specs/004-tokio-async-integration/quickstart.md` |
| Contracts | `/home/noah/Desktop/rudo/specs/004-tokio-async-integration/contracts/` |

**Next Phase**: Run `/speckit.tasks` to generate implementation tasks from this plan.
