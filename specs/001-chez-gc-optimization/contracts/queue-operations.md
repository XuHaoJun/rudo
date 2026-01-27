# Queue Operations Contract

## PerThreadMarkQueue API

### `push_local(work: MarkWork)`

Pushes work to the local queue (LIFO order).

**Preconditions**:
- Caller holds no locks
- Queue capacity below maximum

**Postconditions**:
- Work is visible to local worker
- Work may be stolen by other workers

**Thread Safety**:
- Internal queue uses lock-free or minimally-contended operations

---

### `push_remote(owner: &PerThreadMarkQueue, work: MarkWork)`

Pushes work to another worker's pending queue.

**Preconditions**:
- Caller has identified work item as remote reference
- `owner` is the page owner for this work

**Postconditions**:
- Work added to owner's `pending_work`
- Owner notified via `work_available.notify()`

**Thread Safety**:
- Acquires `pending_work` lock
- Calls `work_available.notify()` without holding lock

---

### `receive_work(&self) -> Vec<MarkWork>`

Drains all pending work from remote workers.

**Preconditions**:
- Local queue is empty
- Worker is idle

**Postconditions**:
- All pending work returned
- `pending_work` cleared

**Thread Safety**:
- Acquires `pending_work` lock
- Uses `std::mem::take` to atomically clear

---

### `try_steal_work(victim: &PerThreadMarkQueue) -> Option<MarkWork>`

Attempts to steal work from another queue (FIFO order).

**Preconditions**:
- Caller's local queue is empty
- Caller has attempted receive_work()

**Postconditions**:
- If successful, work transferred to caller
- If failed, returns None

**Thread Safety**:
- May retry on contention
- Follows Chase-Lev deque protocol

---

### `try_steal_owned_work(thread_id: ThreadId) -> Option<MarkWork>`

Attempts to steal work from page owner's queue first.

**Preconditions**:
- Local queue is empty
- `thread_id` is a known page owner

**Postconditions**:
- Prioritizes stealing from owned pages
- Falls back to general stealing

**Thread Safety**:
- Uses `get_owned_queues()` filter

---

## Lock Ordering Contract

### Acquisition Order (must be strictly followed)

1. **LocalHeap** lock (order 1)
2. **GlobalMarkState** lock (order 2)
3. **GC Request** lock (order 3)

### Forbidden Patterns

- Never acquire LocalHeap while holding GlobalMarkState
- Never acquire GlobalMarkState while holding GC Request
- Never acquire any lock while holding a PerThreadMarkQueue lock

### Validation (Debug Builds Only)

```rust
const LOCK_ORDER_LOCAL_HEAP: u8 = 1;
const LOCK_ORDER_GLOBAL_MARK: u8 = 2;
const LOCK_ORDER_GC_REQUEST: u8 = 3;

fn acquire_lock(tag: u8, expected_min: u8) {
    debug_assert!(
        tag >= expected_min,
        "Lock ordering violation: expected order >= {}, got {}",
        expected_min,
        tag
    );
}
```

---

## Page Ownership Contract

### Ownership Assignment

```rust
impl PageHeader {
    fn set_owner(&mut self, thread_id: ThreadId) {
        self.owner_thread = thread_id;
    }

    fn get_owner(&self) -> ThreadId {
        self.owner_thread
    }
}
```

### Ownership Tracking

```rust
impl PerThreadMarkQueue {
    fn add_owned_page(&mut self, page_ptr: PagePtr) {
        self.owned_pages.insert(page_ptr);
    }

    fn remove_owned_page(&mut self, page_ptr: PagePtr) {
        self.owned_pages.remove(&page_ptr);
    }
}
```

---

## Mark Bitmap Contract

### Bit Operations

```rust
impl MarkBitmap {
    fn mark(&mut self, slot_index: usize) {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        self.bitmap[word] |= 1 << bit;
        self.marked_count.fetch_add(1, Ordering::SeqCst);
    }

    fn is_marked(&self, slot_index: usize) -> bool {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        (self.bitmap[word] >> bit) & 1 != 0
    }

    fn clear(&mut self) {
        for word in &mut self.bitmap {
            *word = 0;
        }
        self.marked_count.store(0, Ordering::SeqCst);
    }
}
```

### Memory Layout

- One bit per pointer-sized unit (8 bytes on 64-bit systems)
- 4KB page = 512 pointer slots = 512 bits = 64 bytes bitmap
- Bitmap aligned to 64-byte boundary for cache efficiency
