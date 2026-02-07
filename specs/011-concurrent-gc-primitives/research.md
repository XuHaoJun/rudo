# Research: Concurrent GC Primitives

**Feature**: 011-concurrent-gc-primitives | **Date**: 2026-02-08

## Decision: Use `parking_lot` for Lock Implementation

**Chosen**: `parking_lot` crate (owned by trusted Rust ecosystem)

**Rationale**:
1. `data_ptr()` method available on `RwLock` and `Mutex` types, enabling lock-free GC tracing
2. Superior performance (~2x faster) than std synchronization primitives due to fewer atomic operations
3. Smaller memory footprint (no state bookkeeping beyond the lock word)
4. Cross-platform support (Linux, macOS, Windows, x86_64, aarch64)
5. Active maintenance and security audits

**Alternatives considered**:
- std::sync::Mutex/RwLock: No `data_ptr()` equivalent, cannot safely implement lock bypass without internal API access
- spin::Mutex: No GC tracing support, not suitable for blocking I/O scenarios
- custom lock implementation: High development cost, potential for subtle bugs

## Decision: STW Lock Bypass Pattern

**Chosen**: Direct pointer dereference during Trace without acquiring lock

**Rationale**:
1. GC runs during global STW pause - all mutator threads are suspended
2. No concurrent mutation during trace - safe to read without lock
3. Atomicity of pointer writes ensures consistent reads even if thread suspended mid-write
4. Proven pattern used in production GC implementations (Go, Java, .NET)

**Safety proof**:
1. **Atomicity**: Pointer writes are atomic on all supported platforms
2. **Exclusivity**: STW ensures no other thread is writing during trace
3. **Visibility**: STW barriers ensure memory visibility before marking

## Decision: Split Type Hierarchy (GcCell vs GcRwLock/GcMutex)

**Chosen**: Separate types for single-threaded vs multi-threaded use cases

**Rationale**:
1. Matches Rust's standard library pattern (Cell vs Mutex)
2. Zero-overhead for single-threaded use cases (no atomics in GcCell)
3. Clear API contract for users about synchronization costs
4. Performance isolation between use cases

## Implementation: Write Barrier Integration

**Generational Barrier**: Add page to thread-local dirty list on write guard acquisition
**SATB Barrier**: Record old value before modification when incremental GC is enabled

Both barriers triggered on guard acquisition (not during writes) to minimize per-operation overhead.

## Dependencies Verified

| Dependency | Version | Status |
|-----------|---------|--------|
| parking_lot | 0.12+ | Available via Cargo.toml |
| Trace trait | Existing | Extends to sync types |
| Write barriers | Existing | Reuse from cell.rs |
| STW mechanism | Existing | Confirmed safe for lock bypass |