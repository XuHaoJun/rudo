# Implementation Plan: Concurrent GC Primitives

**Branch**: `011-concurrent-gc-primitives` | **Date**: 2026-02-08 | **Spec**: [link](/home/noah/Desktop/rudo/specs/011-concurrent-gc-primitives/spec.md)
**Input**: Feature specification from `/specs/011-concurrent-gc-primitives/spec.md`

## Summary

Implement thread-safe concurrent GC primitives (`GcRwLock` and `GcMutex`) using `parking_lot` synchronization primitives, enabling garbage-collected objects to be safely shared across threads while bypassing locks during GC STW pauses to prevent deadlocks.

## Technical Context

**Language/Version**: Rust 1.75+ (stable)  
**Primary Dependencies**: `parking_lot` crate, existing `Trace` trait, existing write barrier infrastructure, existing STW pause mechanism  
**Storage**: N/A (in-memory GC heap)  
**Testing**: cargo test, Miri for unsafe code, ThreadSanitizer for data race detection  
**Target Platform**: Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64  
**Project Type**: Rust library crate (rudo-gc)  
**Performance Goals**: O(1) allocation, minimal pause overhead, atomic synchronization overhead only  
**Constraints**: <1ms additional latency for lock operations, must not regress GcCell performance, must not deadlock during GC STW  
**Scale/Scope**: Enable multi-threaded GC workloads; maintain single-threaded GcCell for DOM/AST use cases

## Constitution Check

| Gate | Status | Notes |
|------|--------|-------|
| I. Memory Safety | REQ | All unsafe code in `sync.rs` must have SAFETY comments; `Trace` bypass implementation must be verified by Miri |
| II. Testing Discipline | REQ | Unit tests for all APIs; integration tests for concurrent access; Miri tests for unsafe pointer dereference |
| III. Performance-First | REQ | Verify GcCell has no atomic overhead; GcRwLock/GcMutex overhead limited to parking_lot synchronization |
| IV. API Consistency | REQ | Follow stdlib naming (`read`, `write`, `lock`); RAII guards with Drop; doc comments with examples |
| V. Cross-Platform | REQ | parking_lot is cross-platform; verify `data_ptr()` availability on all platforms |

## Project Structure

### Documentation (this feature)

```text
specs/011-concurrent-gc-primitives/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output (API specs)
└── tasks.md             # Phase 2 output
```

### Source Code (repository root)

```text
crates/rudo-gc/src/
├── lib.rs               # Add re-exports for GcRwLock, GcMutex
├── cell.rs              # Existing GcCell (unchanged)
└── sync.rs              # NEW: GcRwLock, GcMutex implementations

tests/
├── unit/                # Unit tests for sync primitives
└── integration/         # Concurrent access + GC safety tests
```

## Project Structure

### Documentation (this feature)

```text
specs/[###-feature]/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (/speckit.plan command)
├── data-model.md        # Phase 1 output (/speckit.plan command)
├── quickstart.md        # Phase 1 output (/speckit.plan command)
├── contracts/           # Phase 1 output (/speckit.plan command)
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created by /speckit.plan)
```

### Source Code (repository root)
<!--
  ACTION REQUIRED: Replace the placeholder tree below with the concrete layout
  for this feature. Delete unused options and expand the chosen structure with
  real paths (e.g., apps/admin, packages/something). The delivered plan must
  not include Option labels.
-->

```text
# [REMOVE IF UNUSED] Option 1: Single project (DEFAULT)
src/
├── models/
├── services/
├── cli/
└── lib/

tests/
├── contract/
├── integration/
└── unit/

# [REMOVE IF UNUSED] Option 2: Web application (when "frontend" + "backend" detected)
backend/
├── src/
│   ├── models/
│   ├── services/
│   └── api/
└── tests/

frontend/
├── src/
│   ├── components/
│   ├── pages/
│   └── services/
└── tests/

# [REMOVE IF UNUSED] Option 3: Mobile + API (when "iOS/Android" detected)
api/
└── [same as backend above]

ios/ or android/
└── [platform-specific structure: feature modules, UI flows, platform tests]
```

**Structure Decision**: [Document the selected structure and reference the real
directories captured above]

## Complexity Tracking

No Constitution violations requiring justification.

## Phase 0: Research Findings

**Research completed during spec analysis - no external research needed.**

Key findings consolidated from existing codebase and parking_lot documentation:

- **Decision**: Use `parking_lot` for lock implementations
- **Rationale**: `parking_lot` provides `data_ptr()` method for lock-free GC tracing, has better performance than std Sync primitives, and is cross-platform
- **Alternatives considered**: std `Mutex`/`RwLock` (no `data_ptr()`, cannot implement lock bypass safely)

## Phase 1: Design Artifacts

### Generated Files
- `research.md` - Research findings
- `data-model.md` - Type definitions and relationships
- `quickstart.md` - Usage guide
- `contracts/api.txt` - API specifications

## Status

**Planning Complete** - Ready for Phase 2 (task breakdown)
