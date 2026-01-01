# Data Model: Rust Dumpster GC

**Feature Branch**: `001-rust-dumpster-gc`  
**Date**: 2026-01-02

## Overview

This document defines the key entities and data structures for the garbage collector implementation.

---

## Core Entities

### 1. Gc<T> - User-Facing Smart Pointer

**Purpose**: The primary interface for users to allocate and access garbage-collected objects.

**Fields**:
| Field | Type | Description |
|-------|------|-------------|
| `ptr` | `Cell<Nullable<GcBox<T>>>` | Pointer to the heap-allocated box. Nullable to support "dead" Gc during Drop. |

**Invariants**:
- If `ptr` is null, the Gc is "dead" (only observable during Drop of cyclic structures)
- Cloning increments ref count; dropping decrements and may trigger collection

**Relationships**:
- Points to exactly one `GcBox<T>`
- Multiple `Gc<T>` can point to the same `GcBox<T>` (shared ownership)

---

### 2. GcBox<T> - Heap Allocation Container

**Purpose**: The actual heap allocation wrapping the user's value.

**Fields**:
| Field | Type | Description |
|-------|------|-------------|
| `ref_count` | `Cell<NonZeroUsize>` | Current reference count (for amortized collection triggering) |
| `value` | `T` | The user's data |

**Layout** (C repr):
```rust
#[repr(C)]
struct GcBox<T: Trace + ?Sized> {
    ref_count: Cell<NonZeroUsize>,
    value: T,
}
```

**Invariants**:
- `ref_count >= 1` while any `Gc<T>` points to it
- Located within a `Segment` page, aligned to `BLOCK_SIZE`

---

### 3. PageHeader - BiBOP Page Metadata

**Purpose**: Metadata at the start of each 4KB page, enabling O(1) object lookup.

**Fields**:
| Field | Type | Size | Description |
|-------|------|------|-------------|
| `magic` | `u32` | 4 | Magic number to validate this is a GC page |
| `block_size` | `u16` | 2 | Size of each object slot in bytes |
| `obj_count` | `u16` | 2 | Maximum number of objects in this page |
| `generation` | `u8` | 1 | Generation index (for future generational GC) |
| `flags` | `u8` | 1 | Bitflags (is_large_object, is_dirty, etc.) |
| `_padding` | `[u8; 6]` | 6 | Alignment padding |
| `mark_bitmap` | `[AtomicU64; N]` | varies | One bit per object slot |
| `free_list_head` | `Option<u16>` | 2 | Index of first free slot (for free-list allocation) |

**Computed Values**:
- `header_size`: Size of PageHeader rounded up to `block_size` alignment
- `data_start`: `page_addr + header_size`
- `max_objects`: `(PAGE_SIZE - header_size) / block_size`

**Magic Number**: `0x52554447` ("RUDG" in ASCII)

---

### 4. Segment<const BLOCK_SIZE: usize> - Size-Class Memory Pool

**Purpose**: Manages pages of a specific size class.

**Fields**:
| Field | Type | Description |
|-------|------|-------------|
| `pages` | `Vec<NonNull<PageHeader>>` | All pages in this segment |
| `current_page` | `Option<NonNull<PageHeader>>` | Page currently being allocated from |
| `bump_ptr` | `*mut u8` | Bump pointer for fast allocation |
| `bump_end` | `*const u8` | End of allocatable region |

**Size Classes**:
| Class | Block Size | Max Objects/Page | Use For |
|-------|------------|------------------|---------|
| 0 | 16 | ~252 | Small primitives, Option<Gc<_>> |
| 1 | 32 | ~126 | Common structs (2-4 fields) |
| 2 | 64 | ~62 | Medium structs |
| 3 | 128 | ~31 | Larger structs |
| 4 | 256 | ~15 | Complex objects |
| 5 | 512 | ~7 | Large objects |
| 6 | 1024 | ~3 | Very large objects |
| 7 | 2048 | ~1 | Near-page-size objects |

---

### 5. GlobalHeap - Central Memory Manager

**Purpose**: Coordinates all segments and manages the overall heap.

**Fields**:
| Field | Type | Description |
|-------|------|-------------|
| `segments` | `[Segment; 8]` | One segment per size class |
| `large_objects` | `Vec<NonNull<PageHeader>>` | Pages for objects > 2KB |
| `heap_start` | `*const u8` | Lowest address in heap (for conservative scanning) |
| `heap_end` | `*const u8` | Highest address in heap |

**Thread-Local Access**:
```rust
thread_local! {
    static HEAP: RefCell<GlobalHeap> = RefCell::new(GlobalHeap::new());
}
```

---

### 6. ShadowStack - Root Tracking

**Purpose**: Tracks all active `Gc<T>` roots on the stack.

**Fields**:
| Field | Type | Description |
|-------|------|-------------|
| `roots` | `Vec<NonNull<GcBox<()>>>` | Type-erased pointers to all active roots |
| `frame_markers` | `Vec<usize>` | Stack indices for scope-based rooting |

**Operations**:
- `push(ptr)`: Register a new root
- `pop(ptr)`: Unregister a root
- `iter()`: Iterate all roots for marking phase

---

### 7. CollectInfo - Collection Statistics

**Purpose**: Provides information to the collection condition function.

**Fields**:
| Field | Type | Description |
|-------|------|-------------|
| `n_gcs_dropped` | `usize` | Gc pointers dropped since last collection |
| `n_gcs_existing` | `usize` | Total Gc pointers currently alive |
| `heap_size` | `usize` | Total bytes allocated in heap |
| `last_collect_time` | `Option<Duration>` | Time spent in last collection |

---

## State Transitions

### Gc<T> Lifecycle

```
               ┌─────────────────────┐
               │                     │
   Gc::new()   │   ALIVE             │  .clone()
   ─────────>  │   (ptr is valid)    │ ─────────>  [creates new Gc]
               │                     │
               └──────────┬──────────┘
                          │
                          │ drop() & ref_count == 0
                          │ & object unreachable
                          v
               ┌─────────────────────┐
               │   DEAD              │
               │   (ptr is null)     │  (only during Drop of cycles)
               │                     │
               └─────────────────────┘
```

### Page Lifecycle

```
    alloc_page()         fill up            all objects dead
   ────────────>  ACTIVE ──────────> FULL ─────────────────> FREE
                    ^                  │                       │
                    │                  │ some objects die      │
                    │                  v                       │
                    │               PARTIAL ───────────────────┘
                    │                  │        sweep
                    └──────────────────┘
```

---

## Entity Relationship Diagram

```
┌─────────────────┐         ┌─────────────────┐
│     Gc<T>       │ ──────> │    GcBox<T>     │
│  (user-facing)  │   1:1   │  (heap alloc)   │
└─────────────────┘   ptr   └────────┬────────┘
                                     │
                                     │ located in
                                     v
┌─────────────────┐         ┌─────────────────┐
│  PageHeader     │ <────── │   Segment       │
│  (per page)     │   owns  │  (size class)   │
└────────┬────────┘         └────────┬────────┘
         │                           │
         │ part of                   │ managed by
         v                           v
┌─────────────────────────────────────────────┐
│               GlobalHeap                     │
│         (thread-local singleton)             │
└─────────────────────────────────────────────┘
                     ^
                     │ during collection
                     │
┌─────────────────────────────────────────────┐
│              ShadowStack                     │
│         (provides roots for marking)         │
└─────────────────────────────────────────────┘
```

---

## Validation Rules

1. **Non-null GcBox**: A `GcBox<T>` pointer within a live `Gc<T>` must always be non-null and point to valid memory.

2. **Page Alignment**: All pages must be aligned to `PAGE_SIZE` (4096 bytes).

3. **Block Alignment**: All `GcBox<T>` allocations within a page must be aligned to `block_size`.

4. **Ref Count Consistency**: `ref_count` must equal the number of `Gc<T>` handles pointing to the `GcBox<T>`.

5. **Mark Bitmap Size**: `mark_bitmap` must have enough bits to cover all possible objects in the page.

6. **Magic Number**: PageHeader.magic must equal `MAGIC_GC_PAGE` for valid GC pages.
