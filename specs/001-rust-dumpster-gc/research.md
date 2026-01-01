# Research: Rust Dumpster GC

**Feature Branch**: `001-rust-dumpster-gc`  
**Date**: 2026-01-02  
**Status**: Complete

## Overview

This document captures research findings for implementing a Rust garbage collector inspired by `dumpster` with internal algorithms based on Chez Scheme's BiBOP and Mark-Sweep approach.

---

## 1. Memory Layout Strategy

### Decision: BiBOP (Big Bag of Pages)

**Rationale**: BiBOP provides O(1) allocation, O(1) interior pointer resolution, and natural fragmentation resistance.

**Alternatives Considered**:
| Alternative | Rejected Because |
|-------------|-----------------|
| Traditional malloc/free | Cannot efficiently determine object boundaries for GC scanning |
| Slab allocator only | Doesn't provide the page alignment needed for fast pointer filtering |
| Handle-based indirection | Extra dereference overhead, breaks Rust's `&T` ergonomics |

**Key Implementation Details (from Chez Scheme IMPLEMENTATION.md)**:

1. **Page Alignment**: Pages are 4KB aligned. Given any pointer `P`:
   ```rust
   const PAGE_SIZE: usize = 4096;
   const PAGE_MASK: usize = !(PAGE_SIZE - 1);
   let page_addr = ptr & PAGE_MASK;
   ```

2. **Interior Pointer Resolution**:
   ```rust
   // From page header, get block size
   let block_size = header.block_size;
   let header_size = size_of::<PageHeader>();
   let data_offset = offset - header_size;
   let obj_index = data_offset / block_size;
   ```

3. **Mark Bitmap**: Stored in page header, not per-object. For 4KB page with 32-byte blocks:
   - Max 128 objects → 128 bits → 16 bytes (2 × u64)

**Source**: Chez Scheme `c/gc.c`, `IMPLEMENTATION.md` lines 312-379

---

## 2. Root Tracking Strategy

### Decision: Shadow Stack (MVP), Conservative Scanning (Future)

**Rationale**: Shadow stack is safer to implement in pure Rust, doesn't require platform-specific assembly. Conservative scanning can be added later as optimization.

**Alternatives Considered**:
| Alternative | Rejected Because |
|-------------|-----------------|
| Conservative scanning only | Complex, requires assembly for register spilling, false positives can leak memory |
| Compiler stack maps | Requires rustc modifications, not feasible without language support |
| No explicit root tracking | Would require always-on reference counting (defeats purpose of tracing GC) |

**Shadow Stack Design (from John McCarthy doc)**:

```rust
thread_local! {
    static ROOTS: RefCell<Vec<*const GcBox<()>>> = RefCell::new(Vec::new());
}

// RAII guard for root registration
struct RootGuard {
    ptr: *const GcBox<()>,
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        ROOTS.with(|roots| {
            roots.borrow_mut().retain(|&p| p != self.ptr);
        });
    }
}
```

**Optimization Path**: "Scope-based rooting" where a `gc_scope!` macro batches root registration.

**Source**: John McCarthy doc lines 806-843

---

## 3. Collection Algorithm

### Decision: Mark-Sweep (Non-Moving)

**Rationale**: Non-moving is required to maintain Rust's pointer stability (`&T` must remain valid while borrowed). Mark-Sweep handles cycles naturally.

**Alternatives Considered**:
| Alternative | Rejected Because |
|-------------|-----------------|
| Copying/Semispace | Moves objects, breaks Rust's `&T` safety guarantees |
| Reference counting | Cannot handle cycles without supplementary tracing |
| Generational copying | Same movement problem; also more complex |

**Algorithm (from Chez Scheme gc.c)**:

1. **Mark Phase**:
   - Clear all mark bits
   - For each root, call `mark_object()`
   - `mark_object()` sets the bit in page header bitmap, then traces children

2. **Sweep Phase**:
   - Iterate all segments/pages
   - For each page, check mark bitmap
   - Objects with 0 bits → add to free list (or reset bump pointer)
   - If entire page is dead → return to OS

3. **Tri-color Abstraction** (for future concurrent marking):
   - White: Unmarked (potentially garbage)
   - Gray: Marked but children not yet scanned
   - Black: Marked and children scanned

**Source**: Chez Scheme `c/gc.c` lines 23-110

---

## 4. Trace Trait Design

### Decision: Visitor Pattern (from dumpster)

**Rationale**: Allows multiple visitor implementations (Mark, Sweep, Debug) with same Trace trait. Derive macro generates boilerplate.

**Reference Implementation (dumpster)**:

```rust
// From dumpster/src/lib.rs
pub unsafe trait TraceWith<V: Visitor> {
    fn accept(&self, visitor: &mut V) -> Result<(), ()>;
}

pub trait Visitor {
    fn visit_sync<T>(&mut self, gc: &sync::Gc<T>) where T: Trace + Send + Sync + ?Sized;
    fn visit_unsync<T>(&mut self, gc: &unsync::Gc<T>) where T: Trace + ?Sized;
}
```

**Our Simplified Design**:

```rust
pub unsafe trait Trace {
    fn trace(&self, visitor: &mut dyn Visitor);
}

pub trait Visitor {
    fn visit<T: Trace + ?Sized>(&mut self, gc: &Gc<T>);
}
```

**Derive Macro Pattern**:

```rust
#[derive(Trace)]
struct Node {
    value: i32,
    left: Option<Gc<Node>>,
    right: Option<Gc<Node>>,
}

// Expands to:
unsafe impl Trace for Node {
    fn trace(&self, visitor: &mut dyn Visitor) {
        // value: i32 has empty trace
        self.left.trace(visitor);
        self.right.trace(visitor);
    }
}
```

**Source**: dumpster `src/lib.rs` lines 296-340, `dumpster_derive/src/lib.rs`

---

## 5. Thread Safety Model

### Decision: Thread-Local GC (MVP), Concurrent GC (Future)

**Rationale**: Thread-local simplifies implementation (no locking during alloc/collect). Matches dumpster's `unsync` module.

**dumpster's Approach**:
- `unsync::Gc<T>` - `!Send`, `!Sync`, uses thread-local dumpster
- `sync::Gc<T>` - `Send + Sync` where `T: Send + Sync`, uses global AtomicRefCell

**Our MVP**: Only implement thread-local variant initially.

**Source**: dumpster `src/unsync/mod.rs`, `src/sync/mod.rs`

---

## 6. Collection Triggering

### Decision: Configurable Heuristic (from dumpster)

**Default Condition** (from dumpster):
```rust
pub fn default_collect_condition(info: &CollectInfo) -> bool {
    info.n_gcs_dropped_since_last_collect() > info.n_gcs_existing()
}
```

**Rationale**: Amortizes collection cost to O(1) per operation.

**User Override**:
```rust
pub fn set_collect_condition(f: fn(&CollectInfo) -> bool);
```

**Source**: dumpster `src/unsync/mod.rs` lines 185-209

---

## 7. Build System & Dependencies

### Decision: Workspace with Proc-Macro Crate

**Cargo.toml (workspace root)**:
```toml
[workspace]
members = ["crates/rudo-gc", "crates/rudo-gc-derive"]
```

**rudo-gc dependencies**:
- `std` (default)
- `rudo-gc-derive` (optional, default feature "derive")

**rudo-gc-derive dependencies**:
- `proc-macro2`
- `syn` (features = ["full", "derive"])
- `quote`

---

## 8. Testing Strategy

### Decision: Multi-Layer Testing

| Layer | Purpose | Tools |
|-------|---------|-------|
| Unit | Individual components (Segment, PageHeader) | `cargo test` |
| Integration | Cycle collection, Drop ordering | `cargo test --test` |
| Safety | UB detection | `cargo +nightly miri test` |
| Performance | Allocation speed, collection latency | `criterion` benchmarks |

**Key Test Cases** (from dumpster tests):
1. Simple allocation and drop
2. Self-referencing cycle (`Gc::new_cyclic`)
3. Two-node cycle (A ↔ B)
4. Complex graph with multiple entry points
5. Drop order verification
6. Stress test with many allocations

**Source**: dumpster `src/unsync/tests.rs`

---

## Summary of Decisions

| Topic | Decision | Confidence |
|-------|----------|------------|
| Memory Layout | BiBOP with size classes | High |
| Root Tracking | Shadow Stack (MVP) | Medium |
| GC Algorithm | Non-moving Mark-Sweep | High |
| Trace Trait | Visitor pattern + derive macro | High |
| Thread Safety | Thread-local (MVP) | High |
| Collection Trigger | Configurable heuristic | High |
| Build System | Workspace + proc-macro crate | High |
