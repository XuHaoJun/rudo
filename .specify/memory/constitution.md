<!--
================================================================================
SYNC IMPACT REPORT
================================================================================
Version change: N/A → 1.0.0 (Initial creation)

Added sections:
- I. Memory Safety (NON-NEGOTIABLE)
- II. Testing Discipline (NON-NEGOTIABLE)
- III. Performance-First Design
- IV. API Consistency
- V. Cross-Platform Reliability
- Performance Standards
- Development Workflow
- Governance

Removed sections: N/A (Initial creation)

Templates requiring updates:
- .specify/templates/plan-template.md: ✅ No changes needed (Constitution Check section exists)
- .specify/templates/spec-template.md: ✅ No changes needed (User Scenarios section exists)
- .specify/templates/tasks-template.md: ✅ No changes needed (Testing patterns align)

Follow-up TODOs: None
================================================================================
-->

# rudo-gc Constitution

## Core Principles

### I. Memory Safety (NON-NEGOTIABLE)

All unsafe code MUST have explicit SAFETY comments explaining the contract between the unsafe
operation and its safe callers. Memory safety violations MUST be detected by Miri tests before
merge. The garbage collector MUST never access freed memory or cause undefined behavior. All
GcBox operations MUST maintain Rust's ownership semantics. The marker-based type system
(PhantomData) MUST correctly convey ownership and borrowing properties.

**Rationale**: Memory safety is the foundation of a correct garbage collector. Violations lead
to security vulnerabilities, data corruption, and undefined behavior that Rust's type system
is designed to prevent.

### II. Testing Discipline (NON-NEGOTIABLE)

All new features MUST have corresponding tests before merge. Unsafe code changes MUST pass
Miri tests. GC interference tests MUST use `--test-threads=1` to avoid collection interference
between parallel test threads. Integration tests REQUIRED for: collection correctness, cycle
detection, weak reference lifecycle, and cross-thread behavior (when Send/Sync is added).

**Rationale**: A garbage collector is correctness-critical. Bugs manifest as memory corruption
that is difficult to debug. Comprehensive testing prevents regressions and catches issues
early in development.

### III. Performance-First Design

Performance regressions MUST be detected via benchmarks. Allocation MUST remain O(1) using
BiBOP (Big Bag of Pages) layout. Collection pauses MUST be minimized by respecting the
generational hypothesis (most objects die young). Memory overhead MUST be predictable and
bounded—metadata size MUST be proportional to heap usage, not object count.

**Rationale**: rudo-gc targets high-performance applications. The BiBOP layout and generational
hypothesis are core architectural decisions that enable this. Performance regressions undermine
the project's value proposition.

### IV. API Consistency

The public API MUST follow Rust naming conventions: `snake_case` for functions/methods,
`PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Error handling MUST use
`Result<T, E>` for recoverable errors, reserving `panic!` for unrecoverable programmer errors
(mostly in tests). Naming MUST be consistent with standard library idioms (Gc<T> similar to
Rc<T> and Arc<T>). Public APIs MUST include doc comments with examples.

**Rationale**: Consistent APIs reduce cognitive load for users familiar with Rust patterns.
Predictable naming and behavior improve developer experience and reduce documentation burden.

### V. Cross-Platform Reliability

Conservative stack scanning MUST work correctly on x86_64 and aarch64 across Linux, macOS,
and Windows. Behavior MUST be consistent across platforms—tests MUST pass identically on all
supported platforms. Platform-specific optimizations MUST not break correctness on other
platforms. All platform-specific code MUST be clearly marked.

**Rationale**: A portable garbage collector must behave consistently regardless of platform.
Users expect the same semantics across environments. Platform-specific bugs are costly to
diagnose and fix.

## Performance Standards

### Memory Layout

- BiBOP (Big Bag of Pages) MUST be used for O(1) allocation
- Objects MUST be grouped by size classes into fixed-size pages (typically 4KB)
- Object-to-metadata lookup MUST be O(1) using pointer arithmetic and bit operations
- Page headers MUST contain only essential GC metadata (mark bits, generation info)

### Collection Metrics

- Minor collection pause time MUST be proportional to young generation size
- Major collection pause time MUST be proportional to live heap size
- Allocation rate MUST not be negatively impacted by collection in progress
- Memory fragmentation MUST be minimized through free-list reuse

### Benchmarking

- Performance benchmarks MUST run as part of CI pipeline
- Performance regressions exceeding 10% MUST be investigated before merge
- Benchmark results MUST be tracked over time to detect gradual regressions

## Development Workflow

### Code Quality Gates

All pull requests MUST satisfy:

1. **Lint**: `./clippy.sh` passes with zero warnings
2. **Format**: `cargo fmt --all` produces no changes
3. **Test**: `./test.sh` passes all tests (including ignored)
4. **Safety**: `./miri-test.sh` passes for unsafe code changes
5. **Documentation**: Public APIs have doc comments with examples

### Code Review Requirements

- Unsafe code MUST have SAFETY comments reviewed
- Performance-critical changes MUST include benchmark evidence
- API changes MUST update relevant documentation
- Breaking changes MUST be documented in changelog

### Testing Requirements

- Unit tests in `#[cfg(test)]` modules for internal logic
- Integration tests in `tests/` directory for public API
- Miri tests REQUIRED for any unsafe code involving raw pointers
- Test root registration via `register_test_root()` when Miri cannot find roots

## Governance

This constitution supersedes all other development practices. Amendments require:

1. Documentation of the proposed change and rationale
2. Review and approval by at least one maintainer
3. Migration plan for any breaking changes to existing code
4. Update to all affected templates and documentation

All PRs and code reviews MUST verify compliance with these principles. Complexity that
violates these principles MUST be justified in the PR description with documented rationale
and simpler alternatives that were rejected.

For runtime development guidance, see `AGENTS.md`.

**Version**: 1.0.0 | **Ratified**: 2026-01-27 | **Last Amended**: 2026-01-27
