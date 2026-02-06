# Data Model: Extended GC Metrics System

**Branch**: `010-gc-metrics-v2` | **Date**: 2026-02-06

## 1. Entity Overview

```text
┌──────────────────┐       records to       ┌──────────────────┐
│  Collection Fn   │ ──────────────────────▶ │  record_metrics()│
│  (gc/gc.rs)      │                         │  (choke-point)   │
└──────────────────┘                         └────────┬─────────┘
                                                      │
                              ┌────────────────────────┼────────────────────┐
                              ▼                        ▼                    ▼
                    ┌──────────────────┐   ┌──────────────────┐  ┌──────────────────┐
                    │   LAST_METRICS   │   │  GLOBAL_METRICS  │  │   GC_HISTORY     │
                    │  (thread-local)  │   │  (static atomic) │  │  (static ring)   │
                    │  Cell<GcMetrics> │   │  GlobalMetrics   │  │  GcHistory       │
                    └──────────────────┘   └──────────────────┘  └──────────────────┘

┌──────────────────┐
│    HEAP           │ ◀── current_heap_size(), current_young_size(), current_old_size()
│ (thread-local)    │     (direct reads, no GC involvement)
└──────────────────┘
```

## 2. Entities

### 2.1 `GcMetrics` (Extended)

Per-collection snapshot. One produced per GC cycle. Stored in thread-local `LAST_METRICS` and `GC_HISTORY`.

| Field | Type | Default | Source | Description |
|-------|------|---------|--------|-------------|
| `duration` | `Duration` | `0s` | `Instant::now()` at start/end of collection | Total wall-clock time for the entire collection |
| `bytes_reclaimed` | `usize` | `0` | `before_bytes - after_bytes` | Bytes freed by this collection |
| `bytes_surviving` | `usize` | `0` | `after_bytes` (heap query post-sweep) | Bytes still allocated after collection |
| `objects_reclaimed` | `usize` | `0` | Sum of reclaimed counts from sweep | Number of objects freed |
| `objects_surviving` | `usize` | `0` | `N_EXISTING` counter | Number of objects still alive |
| `collection_type` | `CollectionType` | `None` | Set by collection logic | What kind of collection ran |
| `total_collections` | `usize` | `0` | Set by `record_metrics()` | Monotonic counter for this thread |
| **`clear_duration`** | `Duration` | `0s` | `PhaseTimer::end_clear()` | Time in clear phase (0 for minor) |
| **`mark_duration`** | `Duration` | `0s` | `PhaseTimer::end_mark()` | Time in mark phase (includes all slices for incremental) |
| **`sweep_duration`** | `Duration` | `0s` | `PhaseTimer::end_sweep()` | Time in sweep phase |
| **`objects_marked`** | `usize` | `0` | `MarkStats::objects_marked` | Objects marked (0 for non-incremental) |
| **`dirty_pages_scanned`** | `usize` | `0` | `MarkStats::dirty_pages_scanned` | Dirty pages scanned (0 for STW major) |
| **`slices_executed`** | `usize` | `0` | `MarkStats::slices_executed` | Incremental slices (0 for STW) |
| **`fallback_occurred`** | `bool` | `false` | `MarkStats::fallback_occurred` | Whether incremental fell back to STW |
| **`fallback_reason`** | `FallbackReason` | `None` | `MarkStats::fallback_reason` | Why fallback occurred, if any |

**Bold** = new fields. Existing fields unchanged.

**Traits**: `Debug`, `Clone`, `Copy`, `Default`

**Invariants**:
- `clear_duration + mark_duration + sweep_duration ≤ duration` (inter-phase setup adds overhead)
- `objects_marked > 0` implies `collection_type == IncrementalMajor` or `collection_type == Major`
- `slices_executed > 0` implies `collection_type == IncrementalMajor`
- `fallback_occurred` implies `collection_type == IncrementalMajor`
- If `collection_type == Minor`, then `clear_duration == 0`

### 2.2 `CollectionType` (Unchanged)

```text
None = 0              No collection has run yet
Minor = 1             Young generation only
Major = 2             Full heap STW
IncrementalMajor = 3  Full heap with incremental marking
```

**Change required**: Currently `IncrementalMajor` is never set by collection functions. Fix: set it in `collect_major_incremental()` return path.

### 2.3 `FallbackReason` (Existing, re-exported)

```text
None = 0                No fallback
DirtyPagesExceeded = 1  Too many dirty pages
SliceTimeout = 2        Mark slice timed out
WorklistUnbounded = 3   Worklist grew too large
SatbBufferOverflow = 4  SATB buffer overflow
```

Already defined in `gc::incremental`. Needs re-export through `metrics` module and `lib.rs`.

### 2.4 `PhaseTimer` (Internal, not public)

Internal helper for capturing phase durations.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `clear` | `Duration` | `0s` | Accumulated clear phase time |
| `mark` | `Duration` | `0s` | Accumulated mark phase time |
| `sweep` | `Duration` | `0s` | Accumulated sweep phase time |
| `current_start` | `Option<Instant>` | `None` | Start time of current phase |

**State transitions**:

```text
new() → start() → end_clear() → start() → end_mark() → start() → end_sweep()
         ↑                         ↑                       ↑
    current_start            current_start           current_start
      = Some(now)              = Some(now)             = Some(now)
```

### 2.5 `CollectResult` (Internal, not public)

Return type from `collect_major_stw()` and `collect_major_incremental()`.

| Field | Type | Description |
|-------|------|-------------|
| `objects_reclaimed` | `usize` | Number of objects freed |
| `timer` | `PhaseTimer` | Phase timing data |
| `collection_type` | `CollectionType` | Actual collection type (Major or IncrementalMajor) |

### 2.6 `GlobalMetrics` (New)

Process-level cumulative statistics. Single static instance.

| Field | Type | Ordering | Description |
|-------|------|----------|-------------|
| `total_collections` | `AtomicUsize` | Relaxed | All GC cycles completed |
| `total_minor_collections` | `AtomicUsize` | Relaxed | Minor collections |
| `total_major_collections` | `AtomicUsize` | Relaxed | Major (STW) collections |
| `total_incremental_collections` | `AtomicUsize` | Relaxed | Incremental major collections |
| `total_bytes_reclaimed` | `AtomicUsize` | Relaxed | Cumulative bytes freed |
| `total_objects_reclaimed` | `AtomicUsize` | Relaxed | Cumulative objects freed |
| `total_pause_ns` | `AtomicU64` | Relaxed | Cumulative pause time (nanoseconds) |
| `total_fallbacks` | `AtomicUsize` | Relaxed | STW fallbacks from incremental |

**Access**: `global_metrics() -> &'static GlobalMetrics`

**Storage**: `static GLOBAL_METRICS: GlobalMetrics = GlobalMetrics::new()`

**Write path**: Only in `record_metrics()` — guaranteed single point of entry.

**Read path**: Via accessor methods, callable from any thread at any time.

**Overflow behavior**: `usize` on 64-bit = 18.4 exabytes for bytes, ~584 years at 1 GHz for counters. Overflow is not a practical concern.

### 2.7 `GcHistory` (New)

Fixed-size ring buffer of recent `GcMetrics` snapshots.

| Field | Type | Description |
|-------|------|-------------|
| `buffer` | `UnsafeCell<[GcMetrics; 64]>` | Ring buffer storage |
| `write_idx` | `AtomicUsize` | Monotonically increasing write position |

**Constants**: `HISTORY_SIZE = 64`

**Access**: `gc_history() -> &'static GcHistory`

**Storage**: `static GC_HISTORY: GcHistory = GcHistory::new()`

**Write path**: `push()` called from `record_metrics()` only (single-writer guarantee from GC handshake).

**Read path**: `recent(n)`, `average_pause_time(n)`, `max_pause_time(n)` — callable from any thread.

**Wrap behavior**: When `write_idx >= HISTORY_SIZE`, older entries are overwritten. `recent()` returns newest first, capped to `min(n, total_recorded, HISTORY_SIZE)`.

**Safety**: `unsafe impl Sync for GcHistory` — justified by:
1. Writes serialized by GC handshake (single writer)
2. `write_idx` uses `Release` on write, `Acquire` on read
3. Slot is fully written before index advances

## 3. Relationships

```text
record_metrics(GcMetrics)
    │
    ├──▶ LAST_METRICS.set(metrics)        [thread-local Cell]
    ├──▶ GLOBAL_METRICS.increment(...)    [static atomics]
    └──▶ GC_HISTORY.push(metrics)         [static ring buffer]
```

- **GcMetrics → LAST_METRICS**: 1:1 (latest snapshot per thread)
- **GcMetrics → GlobalMetrics**: N:1 (many snapshots aggregate into one global)
- **GcMetrics → GcHistory**: N:64 (many snapshots, 64 retained)
- **Heap queries**: Independent of GC cycle; read directly from `LocalHeap`

## 4. State Transitions

### 4.1 Collection Lifecycle (with metrics)

```text
                          ┌─────────────────────┐
                          │    GC Triggered      │
                          └──────────┬──────────┘
                                     ▼
                          ┌─────────────────────┐
                          │  start = Instant::now│
                          │  timer = PhaseTimer  │
                          └──────────┬──────────┘
                                     ▼
                    ┌────────────────┴────────────────┐
                    │ Major?                           │ Minor?
                    ▼                                  ▼
           ┌────────────────┐                ┌────────────────┐
           │ timer.start()  │                │ timer.start()  │
           │ Clear phase    │                │ Mark+Sweep     │
           │ timer.end_clear│                │ timer.end_sweep│
           └───────┬────────┘                └───────┬────────┘
                   ▼                                  │
           ┌────────────────┐                         │
           │ timer.start()  │                         │
           │ Mark phase     │                         │
           │ timer.end_mark │                         │
           └───────┬────────┘                         │
                   ▼                                  │
           ┌────────────────┐                         │
           │ timer.start()  │                         │
           │ Sweep phase    │                         │
           │ timer.end_sweep│                         │
           └───────┬────────┘                         │
                   └──────────────┬───────────────────┘
                                  ▼
                       ┌─────────────────────┐
                       │  Build GcMetrics     │
                       │  (timer + MarkStats) │
                       └──────────┬──────────┘
                                  ▼
                       ┌─────────────────────┐
                       │  record_metrics()    │
                       │  → thread-local      │
                       │  → GlobalMetrics     │
                       │  → GcHistory         │
                       └─────────────────────┘
```
