# Data Model: GC Tracing Observability

**Feature**: 009-gc-tracing  
**Date**: 2026-02-05  

## Overview

The tracing feature defines minimal data structures for GC observability. All types are simple value types with Copy semantics for zero-allocation tracing.

## Entities

### GcId

**Purpose**: Stable identifier for correlating all events within a single garbage collection run.

```rust
/// Stable identifier for a GC run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcId(pub u64);
```

**Fields**:
- `0: u64` - Monotonically increasing counter (AtomicU64-backed)

**Invariants**:
- Never zero (starts at 1)
- Unique per collection within process lifetime
- Overflow acceptable (u64 range is effectively infinite for GC frequency)

**Generated Via**:
```rust
static NEXT_GC_ID: AtomicU64 = AtomicU64::new(1);
fn next_gc_id() -> GcId {
    GcId(NEXT_GC_ID.fetch_add(1, Ordering::Relaxed))
}
```

---

### GcPhase

**Purpose**: Categorizes GC operations into high-level phases for trace filtering and analysis.

```rust
/// High-level GC phases (clear/mark/sweep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcPhase {
    Clear,
    Mark,
    Sweep,
}
```

**Variants**:
- `Clear` - Reset mark bits and dirty page tracking
- `Mark` - Trace live object graph
- `Sweep` - Reclaim unreachable objects

**Usage**: Used as span attribute for phase-level tracing and event categorization.

---

### CollectionType

**Purpose**: Existing type from metrics module, reused for trace categorization.

```rust
// From crate::metrics
pub enum CollectionType {
    Minor,
    Major,
}
```

**Mapping to Trace Values**:
- `Minor` → `"minor"`
- `Major` (single-threaded) → `"major_single_threaded"`
- `Major` (multi-threaded) → `"major_multi_threaded"`

---

### Span Types

**GcCollectionSpan**: Top-level span for entire collection
- Name: `"gc_collect"`
- Fields: `collection_type`, `gc_id`
- Level: DEBUG

**GcPhaseSpan**: Phase-level span
- Name: `"gc_phase"`
- Fields: `phase` (as GcPhase)
- Level: DEBUG

**IncrementalMarkSpan**: Incremental marking context
- Name: `"incremental_mark"`
- Fields: `phase` (MarkPhase from incremental module)
- Level: DEBUG

---

## Event Types

### Collection Events

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `phase_start` | DEBUG | phase, bytes_before | Phase entry |
| `phase_end` | DEBUG | phase, bytes_reclaimed, [objects_marked] | Phase exit |

### Incremental Mark Events

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `incremental_start` | DEBUG | budget, gc_id | Slice entry |
| `incremental_slice` | DEBUG | objects_marked, dirty_pages | Slice complete |
| `fallback` | DEBUG | reason | STW fallback triggered |

### Sweep Events

| Event | Level | Fields | When |
|-------|-------|--------|------|
| `sweep_start` | DEBUG | heap_bytes | Sweep entry |
| `sweep_end` | DEBUG | objects_freed, bytes_freed | Sweep exit |

## Relationships

```
GcCollectionSpan (gc_collect)
├── GcPhaseSpan (clear)
│   └── phase_start event
│   └── phase_end event
├── GcPhaseSpan (mark)
│   ├── phase_start event
│   ├── IncrementalMarkSpan(s) [if incremental]
│   │   └── incremental_slice events
│   └── phase_end event
└── GcPhaseSpan (sweep)
    ├── phase_start event
    ├── sweep_start event
    └── sweep_end event
```

## Validation Rules

1. **GcId Uniqueness**: Must be generated atomically, no duplicates
2. **Phase Completeness**: Every collection must have phase_start before phase_end
3. **Span Hierarchy**: Child spans must complete before parent
4. **Event Ordering**: Events must reflect actual execution order

## State Transitions

N/A - Tracing is stateless observation only

## Feature Gate Dependencies

All types and functions in this data model are conditionally compiled:

```rust
#[cfg(feature = "tracing")]
```

This ensures:
- Zero binary size impact when disabled
- Zero runtime overhead when disabled
- Compile-time guarantee of no tracing code paths
