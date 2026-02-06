# Implementation Plan: GC Tracing Observability

**Branch**: `009-gc-tracing` | **Date**: 2026-02-05 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `/specs/009-gc-tracing/spec.md`

## Summary

Add optional `tracing` feature to rudo-gc for structured GC observability using the `tracing` crate. Provides zero-cost spans and events for GC phases (clear, mark, sweep) with per-collection correlation via GcId. Supports all collection types including incremental marking with span propagation across worker threads.

## Technical Context

**Language/Version**: Rust 1.75+ (stable)  
**Primary Dependencies**: `tracing` crate 0.1 (optional), `tracing-subscriber` (dev dependency for testing)  
**Storage**: N/A (in-memory spans, no persistence)  
**Testing**: cargo test with `--test-threads=1` (GC interference), Miri for unsafe code  
**Target Platform**: Linux, macOS, Windows (x86_64, aarch64)  
**Project Type**: Single library (rudo-gc workspace crate)  
**Performance Goals**: Zero overhead when disabled, <1% overhead when enabled but filtered  
**Constraints**: Zero-cost abstraction via `#[cfg(feature = "tracing")]`, DEBUG log level only  
**Scale/Scope**: Single feature module integration (~5 new files, ~200 LOC)

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| **I. Memory Safety** | PASS | No unsafe code required. Tracing uses safe `tracing` crate APIs only. No raw pointer manipulation. |
| **II. Testing Discipline** | PASS | Integration tests required for tracing output verification. Unit tests for span creation. All tests use `--test-threads=1`. |
| **III. Performance-First** | PASS | Zero-cost via feature gates. DEBUG level minimizes overhead. No allocation in hot paths. |
| **IV. API Consistency** | PASS | Follows Rust naming (`GcPhase`, `GcId`). Public API limited to `GcId` re-export. Doc comments required. |
| **V. Cross-Platform** | PASS | `tracing` crate is cross-platform. No platform-specific code required. |

**Code Quality Gates Compliance**:
- Lint: `./clippy.sh` will be run (no new warnings expected)
- Format: `cargo fmt --all` required
- Test: `./test.sh` with tracing feature enabled
- Safety: N/A (no unsafe code in this feature)
- Documentation: Public API (`GcId`) requires doc comments with examples

**Overall Status**: ALL GATES PASS - Proceed with implementation planning

## Project Structure

### Documentation (this feature)

```text
specs/009-gc-tracing/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output - SKIPPED (detailed impl plan exists)
├── data-model.md        # Phase 1 output - types and structures
├── quickstart.md        # Phase 1 output - usage guide
├── contracts/           # Phase 1 output - API surface
└── tasks.md             # Phase 2 output (/speckit.tasks command)
```

### Source Code (repository root)

```text
crates/rudo-gc/
├── Cargo.toml           # Add tracing feature flag
├── src/
│   ├── lib.rs          # Re-export GcId
│   ├── tracing.rs      # NEW: Core tracing types and helpers
│   ├── gc/
│   │   ├── mod.rs      # Add tracing submodule
│   │   ├── tracing.rs  # NEW: GC-specific tracing spans
│   │   ├── gc.rs       # Add collection spans
│   │   ├── incremental.rs  # Add incremental mark spans
│   │   └── sweep.rs    # Add sweep phase spans
│   └── metrics.rs      # CollectionType usage
└── tests/
    └── tracing_tests.rs # NEW: Integration tests for tracing output
```

**Structure Decision**: Single library crate with modular tracing support. New `tracing.rs` at crate root for public types, `gc/tracing.rs` for internal GC-specific spans. Feature-gated compilation ensures zero-cost when disabled.

## Phase 1: Design & Contracts

### Architecture Overview

The tracing feature uses a two-layer architecture:

1. **Public Layer** (`src/tracing.rs`): Core types (`GcPhase`, `GcId`) and span helpers
2. **Internal Layer** (`src/gc/tracing.rs`): GC-specific spans with internal type dependencies

Feature gating via `#[cfg(feature = "tracing")]` ensures zero-cost abstraction - when disabled, no tracing code is compiled.

### Key Design Decisions

1. **DEBUG Level Only**: All spans use `tracing::Level::DEBUG` to avoid spamming INFO logs and minimize overhead
2. **GcId Correlation**: Monotonic counter provides stable identifiers for correlating events within a collection
3. **Span Propagation**: Parent spans captured and entered in worker thread closures for multi-threaded collections
4. **Minimal Public API**: Only `GcId` is re-exported; all other types are internal implementation details

### Integration Points

| Module | Integration | Purpose |
|--------|-------------|---------|
| `gc.rs` | Collection entry points | Top-level `gc_collect` spans |
| `incremental.rs` | Mark slice execution | Incremental mark spans and events |
| `sweep.rs` | Sweep phases | Phase-level sweep spans |
| `lib.rs` | Public re-exports | Expose `GcId` for user correlation |

### Testing Strategy

1. **Unit Tests**: Span creation helpers in `#[cfg(test)]` modules
2. **Integration Tests**: Capture tracing output using `tracing-subscriber` with `tracing-test` crate
3. **Feature Matrix**: Tests run with and without `tracing` feature
4. **Performance**: Verify zero-cost via binary size comparison (with/without feature)

## Constitution Check - Post Design

*Re-evaluated after Phase 1 design completion*

| Principle | Status | Verification |
|-----------|--------|--------------|
| **I. Memory Safety** | PASS | Design uses only safe `tracing` APIs. No unsafe blocks in tracing modules. |
| **II. Testing Discipline** | PASS | Test plan includes unit, integration, and performance tests. Miri compatibility maintained. |
| **III. Performance-First** | PASS | Zero-cost abstraction verified via `#[cfg(feature)]`. DEBUG level prevents overhead. |
| **IV. API Consistency** | PASS | `GcId` follows Rust naming. Doc comments required per API contract. |
| **V. Cross-Platform** | PASS | `tracing` crate supports all target platforms. No platform-specific code. |

**Design Compliance Summary**:
- No constitution violations identified
- All code quality gates can be satisfied
- Agent context updated with new technologies (`tracing` crate)

**Status**: DESIGN APPROVED - Ready for task breakdown (`/speckit.tasks`)
