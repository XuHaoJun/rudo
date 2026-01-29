# rudo-gc 重要設計與算法完整報告

> 調查日期：2026-01-29
> 目的：為下一步大計畫做準備

---

## 1. BiBOP (Big Bag of Pages) 記憶體布局

### 核心概念

- 每個頁面固定 4KB，包含**單一 size-class** 的物件
- Size classes: `[16, 32, 64, 128, 256, 512, 1024, 2048]` bytes
- 超過 2KB 的物件走大型物件分配路徑

### 記憶體佈局

```
┌─────────────────────────────────────┐
│           PageHeader (64 bytes)     │  ← Magic, block_size, bitmaps
├─────────────────────────────────────┤
│  Slot 0: GcBox<T>                   │
│  Slot 1: GcBox<T>                   │
│  ...                                │  ← 4KB / block_size 個槽
│  Slot N: GcBox<T>                   │
└─────────────────────────────────────┘
```

### 設計優勢

- `ptr & page_mask()` 快速獲取頁面起始地址
- 單一頁面內無外部碎片
- O(1) 安全檢查透過 `small_pages` HashSet

### 關鍵程式碼位置

- `crates/rudo-gc/src/heap.rs` - LocalHeap, PageHeader 實現

---

## 2. TLAB (Thread-Local Allocation Buffer)

### 結構

```rust
pub struct Tlab {
    pub bump_ptr: *mut u8,           // 下一次分配位置
    pub bump_end: *const u8,         // 當前頁面結束
    pub current_page: Option<NonNull<PageHeader>>,
}
```

### 分配層級

| 優先級 | 方式 | 同步 | 速度 |
|--------|------|------|------|
| 1 | TLAB 分配 (Bump-pointer) | 無鎖 | 最快 |
| 2 | Free List 分配 | 頁面內 | 中等 |
| 3 | 新頁面分配 | 全域鎖 | 最慢 |

---

## 3. Mark-Sweep GC 算法

### 收集觸發條件

```rust
pub const fn default_collect_condition(info: &CollectInfo) -> bool {
    info.n_gcs_dropped > info.n_gcs_existing ||  // 丟棄數 > 現有數
    info.young_size > 1024 * 1024                // 年輕代 > 1MB
}
```

**設計原理**：確保攤銷 O(1) 的### 三階段 Major GC（解決收集開銷。

跨堆引用問題）

```
Phase 1: 清除所有堆上所有物件的標記
    ↓
Phase 2: 標記所有可達物件（跨所有堆）
    ↓
Phase 3: 清除所有堆 + promotion
```

**關鍵修復**：舊方法獨立處理每個堆，導致跨堆引用被錯誤清除。

### 兩階段 Sweep

1. **Phase 1**: 執行 Drop 函數，確保清除時其他 GC 物件仍可訪問（防止 Use-After-Free）
2. **Phase 2**: 回收記憶體到 free list

### 關鍵程式碼位置

- `crates/rudo-gc/src/gc/gc.rs` - Mark-Sweep 實現

---

## 4. 標記點陣圖 (Mark Bitmap)

### 每頁點陣圖結構

```rust
pub struct PageHeader {
    pub mark_bitmap: [AtomicU64; BITMAP_SIZE],    // 標記狀態
    pub dirty_bitmap: [AtomicU64; BITMAP_SIZE],   // 寫入 barrier
    pub allocated_bitmap: [u64; BITMAP_SIZE],     // 分配狀態（非原子）
}
```

### 記憶體效率

| 方案 | 每物件開銷 | 4KB 頁面總開銷 |
|------|-----------|----------------|
| 轉發指標 | 8 bytes | 4096 bytes |
| 標記點陣圖 | 1 bit | 64 bytes |

**減少 98% 每物件開銷**

### 原子標記操作

```rust
pub unsafe fn mark(&self, slot_index: usize) {
    let word = slot_index / 64;
    let mask = 1u64 << bit;
    let prev = self.bitmap[word].fetch_or(mask, Ordering::SeqCst);
    if prev & mask == 0 {  // 冪等標記
        self.marked_count.fetch_add(1, Ordering::SeqCst);
    }
}
```

**優化建議 (RLC 建議)**：考慮在 TLAB 或本地緩衝區進行非原子標記，最後再一次性合併到全域 Bitmap，以減少 Cache Coherence 流量。

### 關鍵程式碼位置

- `crates/rudo-gc/src/gc/mark/bitmap.rs` - MarkBitmap 實現

---

## 5. 並行標記與工作竊取

### Chase-Lev 無鎖工作竊取佇列

```rust
pub struct StealQueue<T: Copy, const N: usize> {
    buffer: UnsafeCell<[MaybeUninit<T>; N]>,
    bottom: AtomicUsize,  // 生產者 (LIFO - 本地操作)
    top: AtomicUsize,     // 消費者 (FIFO - 竊取操作)
    mask: usize,          // N-1 取模
}
```

### 操作特性

| 操作 | 同步方式 | 記憶體順序 | 特性 |
|------|----------|------------|------|
| push | 無 CAS | Relaxed | 本地 LIFO |
| pop | CAS (僅最後元素) | AcqRel/Acquire | 本地 LIFO |
| steal | CAS | Acquire | 遠端 FIFO |

### 推送式工作轉移

```
┌──────────────────────────────────────────────────────────────┐
│  Worker 發現遠端指標                                           │
│       ↓                                                       │
│  緩衝區 < 16 項?                                               │
│   │                                                           │
│   ├── 是 → 加入本地 pending_work                               │
│   │                                                           │
│   └── 否 → push_remote() 到所有者 PerThreadMarkQueue          │
└──────────────────────────────────────────────────────────────┘
```

**設計優勢**：
- 減少竊取競爭（只有一個 pusher 競爭一個 Mutex）
- bounded buffer 防止無限制記憶體增長

### 頁面擁有權追蹤

- 每個 worker 維護 `owned_pages`
- 標記時優先處理自有頁面（快取局部性）
- 竊取時優先選擇有重疊擁有權的佇列

### 關鍵程式碼位置

- `crates/rudo-gc/src/gc/worklist.rs` - Chase-Lev Queue
- `crates/rudo-gc/src/gc/marker.rs` - ParallelMarkCoordinator
- `crates/rudo-gc/src/gc/mark/ownership.rs` - 頁面擁有權

---

## 6. Trace Trait 與 Visitor 模式

### 核心 Trait

```rust
pub unsafe trait Trace {
    fn trace(&self, visitor: &mut impl Visitor);
}

pub trait Visitor {
    fn visit<T: Trace>(&mut self, gc: &Gc<T>);
    unsafe fn visit_region(&mut self, ptr: *const u8, len: usize);
}
```

### Visitor 類型

| 類型 | 用途 |
|------|------|
| `GcVisitor` | 標準迭代標記 |
| `GcVisitorConcurrent` | 並行標記路由 |

### 支援類型

**基本類型**：所有原生類型 (i*, u*, f*, bool, char, ())

**集合**：
- `Vec<T>`, `[T; N]`, `[T]`
- `HashMap<K, V, S>`, `BTreeMap<K, V>`
- `HashSet<T>`, `BTreeSet<T>`
- `VecDeque<T>`, `LinkedList<T>`

**智慧指標**：
- `Box<T>`, `Rc<T>`, `Arc<T>`
- `&T`, `&mut T`

**標準庫**：
- `String`, `str`
- `Option<T>`, `Result<T, E>`
- `Cell<T>`, `RefCell<T>`
- `std::sync::atomic::*`

### 關鍵程式碼位置

- `crates/rudo-gc/src/trace.rs` - Trace trait 定義與實現

---

## 7. Safe Point 與執行緒協調

### 執行緒狀態

```rust
pub const THREAD_STATE_EXECUTING: usize = 0;    // 執行中
pub const THREAD_STATE_SAFEPOINT: usize = 1;    // 安全點
pub const THREAD_STATE_INACTIVE: usize = 2;     // 非活躍
```

### 協作式 Rendezvous 流程

```
┌─────────────────────────────────────────────────────────────────┐
│  執行緒執行中                                                     │
│       ↓                                                         │
│  check_safepoint() → GC_REQUESTED == true?                       │
│       │                                                         │
│       ├── 否 → 繼續執行                                          │
│       │                                                         │
│       └── 是 → enter_rendezvous()                               │
│                   ↓                                             │
│                   1. 狀態: EXECUTING → SAFEPOINT                │
│                   2. 捕獲堆疊根 (spill_registers_and_scan)       │
│                   3. 遞減 active_count                           │
│                   4. park_cond.wait() 等待 GC 完成               │
└─────────────────────────────────────────────────────────────────┘
```

### safepoint() API

提供手動安全點檢查給使用者：

```rust
/// 手動檢查 GC 請求並阻塞直到處理完成。
///
/// 此函數應該在長期執行的迴圈中呼叫（不進行分配的情況下），
/// 以確保執行緒能及時響應 GC 請求。
///
/// # 範例
///
/// ```
/// use rudo_gc::safepoint;
///
/// for _ in 0..1000 {
///     // 做一些非分配的工作...
///     let _: Vec<i32> = (0..100).collect();
///
///     // 檢查 GC 請求
///     safepoint();
/// }
/// ```
pub fn safepoint() {
    crate::heap::check_safepoint();
}
```

### 無限迴圈風險

**風險**：如果執行緒進入緊湊計算迴圈（不分配、不呼叫函數），可能永遠不檢查 `GC_REQUESTED`，導致 Stop-Forever。

**緩解措施**：

1. **`safepoint()` 手動檢查**：使用者應在長時間迴圈中手動呼叫
2. **`clear_registers()`**：清除分配器的 registers，避免殘留指標
3. **`MASK` 機制**：頁面地址 XOR MASK 過濾 False Roots
4. **Stack Conflict 檢測**：分配時檢測並隔離問題頁面

```rust
/// 清除 CPU registers 以防止 "False Roots" 殘留。
///
/// 這用於分配器，確保新分配頁面的指標不會殘留在 register 中。
#[inline(never)]
pub unsafe fn clear_registers() {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    unsafe {
        std::arch::asm!(
            "xor r12, r12",
            "xor r13, r13",
            "xor r14, r14",
            "xor r15, r15",
        );
    }
}
```

### Address Space Coloring 與 False Root 過濾

```rust
/// 使用 MASK 隱藏分配器的變數，避免保守堆疊掃描誤判。
const MASK: usize = 0x5555_5555_5555_5555;

fn calculate_masked_range(mmap: &Mmap, size: usize, mask: usize) -> (usize, usize) {
    let ptr = mmap.ptr() as usize;
    (ptr ^ mask, (ptr + size) ^ mask)
}
```

### 關鍵程式碼位置

- `crates/rudo-gc/src/heap.rs` - ThreadRegistry, ThreadControlBlock
- `crates/rudo-gc/src/stack.rs` - 堆疊掃描, clear_registers
- `crates/rudo-gc/src/gc/gc.rs` - safepoint API

---

## 8. 鎖順序紀律 (Deadlock Prevention)

### 嚴格鎖順序

| 順序 | 鎖類型 | 說明 |
|------|--------|------|
| 1 | `LocalHeap` | 執行緒本地分配2 | `Global鎖 |
| MarkState` | 標記狀態協調鎖 |
| 3 | `GcRequest` | GC 請求協調鎖 |

### 驗證機制

```rust
thread_local! {
    static MIN_LOCK_ORDER_STACK: RefCell<Vec<u8>> = ...;
}

// Debug 建置：違反則 panic
// Release 建置：優化掉
pub fn validate_lock_order(tag: LockOrder, expected_min: LockOrder) {
    debug_assert!(
        tag.order_value() >= expected_min.order_value(),
        "Lock ordering violation: ..."
    );
}
```

### 關鍵程式碼位置

- `crates/rudo-gc/src/gc/sync.rs` - 鎖順序驗證

---

## 9. Gc<T> 指標實現

### GcBox 結構

```rust
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: AtomicUsize,           // 強引用計數
    weak_count: AtomicUsize,          // 弱引用計數（含標誌位）
    drop_fn: unsafe fn(*mut u8),      // 型別擦除的 drop
    trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    is_dropping: AtomicUsize,         // 防止 Drop/Weak race
    value: T,
}
```

### Header 開銷

| 元件 | 大小 |
|------|------|
| ref_count | 8 bytes |
| weak_count | 8 bytes |
| drop_fn | 8 bytes |
| trace_fn | 8 bytes |
| is_dropping | 8 bytes |
| **總計** | **40 bytes** |

**優化建議 (RLC 建議)**：考慮將 `drop_fn` 和 `trace_fn` 移入靜態 `VTable`，在 `GcBox` 中只存一個指向 `VTable` 的指標。這可以節省 8-16 bytes。

### 標誌位設計

```rust
const DEAD_FLAG: usize = 1 << (usize::BITS - 1);           // 值已釋放
const UNDER_CONSTRUCTION_FLAG: usize = 1 << (usize::BITS - 2); // 建構中
```

### Send/Sync 條件

```rust
unsafe impl<T: Trace + Send + Sync> Send for Gc<T> {}
unsafe impl<T: Trace + Send + Sync> Sync for Gc<T> {}
```

### ZST (Zero-Sized Type) 處理

```rust
static ZST_SINGLETON: AtomicPtr<GcBox<()>> = AtomicPtr::new(std::ptr::null_mut());
// CAS 初始化 singleton，永遠不回收
```

### 關鍵程式碼位置

- `crates/rudo-gc/src/ptr.rs` - Gc<T>, Weak<T>, GcBox 實現

---

## 10. Weak 引用處理

### 設計特點

- Weak 不參與追蹤（`unsafe impl Trace for Weak<T>` 為空）
- `upgrade()` 檢查 `is_value_dead()` 和 `is_dropping()`
- 循環引用檢測：當只剩 Weak 時，呼叫 `mark_dead()`

### Race 預防

```rust
fn try_mark_dropping(&self) -> bool {
    self.is_dropping
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
}
```

### 關鍵程式碼位置

- `crates/rudo-gc/src/ptr.rs` - Weak<T> 實現

---

## 11. 保守堆疊掃描

### 設計特點

```rust
unsafe fn scan_heap_region_conservatively(
    region_ptr: *const u8,
    region_len: usize,
    visitor: &mut GcVisitor,
) {
    // 對齊到 usize 邊界
    // 掃描每個指標大小的 word
    // 呼叫 find_gc_box_from_ptr 驗證是否為 GC 指標
}
```

**特性**：
- 保守掃描：可能將整數誤識別為指標（導致記憶體膨脹）
- 用於處理無法靜態追蹤的根（如 native stack）

### 限制

1. **False Positive 風險**：整數可能被誤識別為指標，導致記憶體膨脹
2. **無法實現 Compaction**：保守掃描無法更新堆疊上的「疑似指標」，因此無法實現移動式 GC
3. **無法實現 Precise GC**：需要編譯器支持或過程巨集才能實現精確掃描

### 緩解機制

1. **`MASK` 機制**：頁面地址 XOR MASK 過濾 False Roots
2. **`clear_registers()`**：清除分配器的 registers
3. **Stack Conflict 檢測**：分配時檢測並隔離問題頁面

### 關鍵程式碼位置

- `crates/rudo-gc/src/scan.rs` - 保守掃描
- `crates/rudo-gc/src/stack.rs` - 堆疊掃描, clear_registers

---

## 12. 世代 GC 支援

### PageHeader 中的世代欄位

```rust
pub generation: u8,  // 0 = young, >0 = old
```

### Minor GC 行為

- 只標記年輕代物件 (`generation == 0`)
- 遇老年代物件停止 (`generation > 0` 跳過)
- 使用 dirty bitmap 追蹤老年代→年輕代引用

### Major GC

- 清除所有標記
- 標記所有可達物件
- 所有標記物件 promotion 到 old generation

---

## 13. 溢位佇列同步

### ABA 問題預防

```rust
const GENERATION_SHIFT: usize = 48;  // 高 16 位儲存世代計數

fn push_overflow_work(work: *const GcBox<()>) -> Result<(), *const GcBox<()>> {
    // 使用 CAS + 世代計數防止 ABA
}
```

### 安全清除協議

```
1. 信號意圖：fetch_add(1) 到 CLEAR_GEN
2. 等待使用者：spin_loop() 直到 USERS == 0
3. 安全清除：pop 所有節點
4. 重置狀態：fetch_add(1) 到 CLEAR_GEN
```

### 關鍵程式碼位置

- `crates/rudo-gc/src/gc/marker.rs` - 溢位佇列

---

## 14. 總結：核心設計模式

| 機制 | 設計選擇 | 目的 |
|------|----------|------|
| 記憶體布局 | BiBOP | 無外部碎片，快速安全檢查 |
| 分配 | TLAB + Bump-pointer | 無鎖快速分配 |
| 收集 | Mark-Sweep + 世代 | 平衡吞吐量與延遲 |
| 標記 | Per-page Bitmap | 98% 減少每物件開銷 |
| 並發 | Chase-Lev Queue | 無鎖工作竊取 |
| 工作路由 | Push-based transfer | 減少竊取競爭 |
| 同步 | 鎖順序紀律 | 防止死鎖 |
| 根追蹤 | 保守堆疊掃描 | 通用指標處理 |
| 指標 | GcBox + 原子操作 | 執行緒安全 |

---

## 15. 專案結構

```
/home/noah/Desktop/rudo/
├── Cargo.toml                 # 工作區根配置
├── AGENTS.md                  # 開發指南
├── crates/
│   ├── rudo-gc/              # 主 GC 庫 (核心)
│   │   └── src/
│   │       ├── lib.rs        # 庫入口
│   │       ├── ptr.rs        # Gc<T>, Weak<T>
│   │       ├── heap.rs       # BiBOP, LocalHeap
│   │       ├── trace.rs      # Trace trait
│   │       ├── scan.rs       # 保守掃描
│   │       └── gc/
│   │           ├── gc.rs     # Mark-Sweep
│   │           ├── marker.rs # 並行標記
│   │           ├── worklist.rs # 工作竊取
│   │           ├── mark/     # 標記點陣圖
│   │           └── sync.rs   # 鎖順序
│   ├── rudo-gc-derive/       # 過程式宏
│   └── sys_alloc/            # 系統分配器
├── docs/                     # 文檔
└── tests/                    # 測試
```

---

## 16. 關鍵演算法複雜度

| 操作 | 時間複雜度 | 空間複雜度 |
|------|-----------|------------|
| TLAB 分配 | O(1) | O(1) |
| GC 觸發檢查 | O(1) | O(1) |
| 標記物件 | O(1) | O(1) |
| Sweep 頁面 | O(n) | O(1) |
| 工作竊取 | Amortized O(1) | O(1) |
| 頁面安全檢查 | O(1) | O(1) |

---

> 本文件基於 2026-01-29 調查生成。如有更新，請同步維護。
