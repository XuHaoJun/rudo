# rudo-gc Send & Sync Trait 實作 - 需求分析與技術規格

**版本**: 1.0  
**日期**: 2026-01-27  
**狀態**: 草稿  
**作者**: R. Kent Dybvig & John McCarthy (Parallel World Collaboration) 技術顧問

---

## 1. 執行摘要

本文件描述為 `rudo-gc` 實現 `Send` 和 `Sync` Trait 的需求與技術規格。基於對程式碼庫的分析，借鏡 `dumpster` 參考實作與 ChezScheme 的 BiBOP GC 設計，提出完整的實作方案。

### 1.1 背景

rudo-gc 目前實作了一個基於 BiBOP (Big Bag of Pages) 的標記-清除垃圾回收器，具有：
- 分代 GC 支援 (年輕代/老年代)
- TLAB (Thread-Local Allocation Buffer) 分配
- STW (Stop-The-World) 收集
- 安全點協議 (safepoint protocol)

目前的限制：`Gc<T>` 故意設計為 `!Send` 和 `!Sync`，以確保單執行緒安全性。

### 1.2 目標

1. 使 `Gc<T>` 在 `T: Send + Sync` 時實現 `Send` 和 `Sync`
2. 為未來並行標記 (parallel marking) 奠定基礎
3. 保持與現有多執行緒 GC 基礎設施的相容性

---

## 2. 現況分析

### 2.1 當前架構

```
crates/rudo-gc/src/
├── lib.rs                 # 公共 API 導出
├── gc.rs                  # Mark-Sweep 收集算法
├── heap.rs                # BiBOP 記憶體管理
├── ptr.rs                 # Gc<T> 和 Weak<T> 實現
├── trace.rs               # Trace trait 和 Visitor 模式
├── cell.rs                # GcCell 內部可變性
├── stack.rs               # 保守式堆疊掃描
├── scan.rs                # 保守式堆區域掃描
├── trace_closure.rs       # 閉包包裝追蹤
└── metrics.rs             # GC 統計
```

### 2.2 GcBox 當前結構 (`ptr.rs:20-33`)

```rust
#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: Cell<NonZeroUsize>,        // 非執行緒安全
    weak_count: Cell<usize>,              // 非執行緒安全
    drop_fn: unsafe fn(*mut u8),
    trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    value: T,
}
```

### 2.3 Gc<T> 當前結構 (`ptr.rs:265-271`)

```rust
pub struct Gc<T: Trace + ?Sized + 'static> {
    ptr: Cell<Nullable<GcBox<T>>>,        // 非執行緒安全
    _marker: PhantomData<*const ()>,      // 強制 !Send + !Sync
}
```

### 2.4 現有多執行緒基礎設施 (`heap.rs`)

| 元件 | 特性 | 狀態 |
|------|------|------|
| `ThreadControlBlock` | 執行緒控制區塊 | 已實現 `Send + Sync` |
| `ThreadRegistry` | 執行緒登錄系統 | 使用 `Mutex` 同步 |
| `GlobalSegmentManager` | 全局記憶體管理 | 已實現 `Send + Sync` |
| 安全點協議 | STW 協調 | 已實現 |

---

## 3. 需求規格

### 3.1 功能性需求

| 編號 | 需求描述 | 優先級 | 說明 |
|------|----------|--------|------|
| REQ-01 | `Gc<T>` 在 `T: Send + Sync` 時實現 `Send` | 高 | 允許跨執行緒共享 |
| REQ-02 | `Gc<T>` 在 `T: Send + Sync` 時實現 `Sync` | 高 | 允許跨執行緒共享引用 |
| REQ-03 | `Weak<T>` 在 `T: Send + Sync` 時實現 `Send` | 中 | 弱引用跨執行緒安全 |
| REQ-04 | `Weak<T>` 在 `T: Send + Sync` 時實現 `Sync` | 中 | 弱引用跨執行緒安全 |
| REQ-05 | 參考計數操作必須是原子的 | 高 | 執行緒安全基礎 |
| REQ-06 | 指標存取必須是原子的 | 高 | 防止競爭條件 |

### 3.2 非功能性需求

| 編號 | 需求描述 | 目標 |
|------|----------|------|
| NFR-01 | 效能開銷最小化 | 原子操作 overhead 最小化 |
| NFR-02 | 記憶體安全 | 所有 unsafe 程式碼必須有 SAFETY 註解 |
| NFR-03 | 可測試性 | 必須通過 Miri 記憶體安全檢查 |
| NFR-04 | 向後兼容性 | 不破壞現有單執行緒 API |

### 3.3 未來擴展需求 (並行標記)

| 編號 | 需求描述 | Scope |
|------|----------|-------|
| EREQ-01 | 原子標記位存取 | 未來 |
| EREQ-02 | 並發工作列表 (worklist) | 未來 |
| EREQ-03 | 執行緒安全 GcCell | 未來 |

---

## 4. 技術規格

### 4.1 設計方案：增量修改

由於不需要向後兼容性，採用增量修改方案，將 `Cell` 替換為原子類型。

#### 4.1.1 GcBox 修改

**檔案**: `crates/rudo-gc/src/ptr.rs`

```rust
#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: AtomicUsize,           // 替換 Cell<NonZeroUsize>
    weak_count: AtomicUsize,          // 替換 Cell<usize>
    drop_fn: unsafe fn(*mut u8),
    trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    value: T,
}
```

#### 4.1.2 Gc<T> 修改

```rust
pub struct Gc<T: Trace + ?Sized + 'static> {
    ptr: AtomicPtr<GcBox<T>>,         // 替換 Cell<Nullable<...>>
}

// 移除 PhantomData<*const ()>，改用 trait bound
```

#### 4.1.3 Trait 實現

```rust
unsafe impl<T: Trace + Send + Sync + ?Sized> Send for Gc<T> {}

unsafe impl<T: Trace + Send + Sync + ?Sized> Sync for Gc<T> {}
```

### 4.2 記憶體順序策略

基於 ChezScheme 的記憶體順序設計 (`learn-projects/ChezScheme/c/atomic.h`):

| 操作 | 建議順序 | x86_64 | ARM64 | 理由 |
|------|----------|--------|-------|------|
| `inc_ref` | `Relaxed` | 無成本 | 低成本 | 僅計數，不涉及同步 |
| `dec_ref` | `AcqRel` | 中成本 | 中成本 | 需要釋放舊記憶體順序 |
| `inc_weak` | `Relaxed` | 無成本 | 低成本 | 弱引用操作 |
| `dec_weak` | `AcqRel` | 中成本 | 中成本 | 需要同步釋放 |
| 指標載入 | `Acquire` | 無成本 | 中成本 | 需要看到完整初始化 |
| 指標儲存 | `Release` | 無成本 | 中成本 | 確保初始化可見 |

#### ChezScheme 參考實作

```c
// x86_64 - 強記憶體模型
#define STORE_FENCE()   __asm__ __volatile__ ("sfence" : : : "memory")
#define ACQUIRE_FENCE() do { } while (0)  // 無需屏障
#define RELEASE_FENCE() do { } while (0)

// ARM64 - 弱記憶體模型
#define STORE_FENCE()   __asm__ __volatile__ ("dmb ishst" : : : "memory")
#define ACQUIRE_FENCE() __asm__ __volatile__ ("dmb ish" : : : "memory")
#define RELEASE_FENCE() ACQUIRE_FENCE()
```

Rust 的 `std::sync::atomic` 已處理這些平台差異。

### 4.3 效能影響評估

| 操作 | 單執行緒 (`Cell`) | 多執行緒 (`Atomic`) | 差距 |
|------|-------------------|---------------------|------|
| `clone()` | ~5 cycles | ~20-50 cycles | 4-10x |
| `drop()` | ~5 cycles | ~25-50 cycles | 5-10x |
| `deref()` | ~2 cycles | ~2-5 cycles | ~1-2x |
| `inc_weak()` | ~3 cycles | ~15-30 cycles | 5-10x |

**說明**：
- 大多數 GC overhead 在配置和追蹤，不在計數操作
- 對於多執行緒程式，這些開銷是可接受的
- x86_64 的原子操作相對較快 (無需記憶體屏障)

### 4.4 為並行標記奠基

#### 4.4.1 標記位存取

ChezScheme 使用每段獨立的標記位圖 (`marked_mask`)：

```c
// c/types.h
typedef struct _seginfo {
  octet *marked_mask;                      // 每段的標記位圖
  struct thread_gc *creator;               // 用於並行 GC
  unsigned char use_marks : 1;             // 是否原地標記
  // ...
} seginfo;
```

rudo-gc 已在 `PageHeader` 中實現類似設計。對於並行標記，需確保 `set_mark` 使用適當的記憶體順序：

```rust
// heap.rs - 建議修改
pub fn set_mark(&self, index: usize) {
    // 並行標記時需要 AcqRel，其他情況 Release 即可
    self.mark_bits.fetch_or(1 << offset, Ordering::Release);
}
```

#### 4.4.2 層次化段表

ChezScheme 使用三級段表結構：

```c
// c/segment.h
#define SEGMENT_T1_SIZE ((uptr)1<<segment_t1_bits)
#define SEGMENT_T2_IDX(i) (((i)>>segment_t1_bits)&(SEGMENT_T2_SIZE-1))
#define SEGMENT_T3_IDX(i) ((i)>>(segment_t2_bits+segment_t1_bits))

static inline seginfo *SegmentInfo(iptr i) {
  return AS_IMPLICIT_ATOMIC(seginfo *, 
    S_segment_info[SEGMENT_T3_IDX(i)]->t2[SEGMENT_T2_IDX(i)]->t1[SEGMENT_T1_IDX(i)]);
}
```

rudo-gc 的 `SegmentManager` 已有類似設計，需確保 Lazy Allocation 時的原子安全性。

### 4.5 API 變更

#### 4.5.1 Trait Bounds 變更

| 類型 | 當前 | 實作後 |
|------|------|--------|
| `Gc<T>` | `T: Trace` | `T: Trace + Send + Sync` (用於 Send/Sync) |
| `Weak<T>` | `T: Trace` | `T: Trace + Send + Sync` (用於 Send/Sync) |

#### 4.5.2 內部 API 變更

| 方法 | 當前 | 實作後 |
|------|------|--------|
| `GcBox::ref_count` | `Cell<NonZeroUsize>` | `AtomicUsize` |
| `GcBox::weak_count` | `Cell<usize>` | `AtomicUsize` |
| `Gc::ptr` | `Cell<Nullable<GcBox<T>>>` | `AtomicPtr<GcBox<T>>` |

---

## 5. 實作範圍

### 5.1 此次 Scope (Send/Sync)

```
crates/rudo-gc/src/
├── ptr.rs
│   ├── GcBox<T>
│   │   ├── ref_count: Cell → AtomicUsize
│   │   ├── weak_count: Cell → AtomicUsize
│   │   └── drop_fn, trace_fn, value: 保持不變
│   │
│   ├── Gc<T>
│   │   ├── ptr: Cell → AtomicPtr
│   │   └── 移除 _marker PhantomData
│   │
│   └── 新增
│       ├── unsafe impl Send for Gc<T>
│       └── unsafe impl Sync for Gc<T>
│
└── heap.rs
    └── 審查 PageHeader::set_mark/get_mark 記憶體順序
```

### 5.2 未來 Scope (並行標記)

```
crates/rudo-gc/src/sync/
├── mod.rs                 # 同步 collector 狀態
├── visitor.rs             # Concurrent GcVisitor
├── mark_bitmap.rs         # 原子標記位圖操作
└── worklist.rs            # 並發工作列表 (MPSC 佇列)
```

---

## 6. 測試策略

### 6.1 單元測試

```rust
#[test]
fn test_gc_send_sync() {
    // 驗證 Send + Sync 類型
    let gc: Gc<Arc<AtomicUsize>> = Gc::new(AtomicUsize::new(0));
    assert_send_and_sync::<Gc<Arc<AtomicUsize>>>();
}

#[test]
fn test_atomic_ref_count() {
    // 跨執行緒 clone/drop 壓力測試
}
```

### 6.2 整合測試

- 多執行緒 `clone()` + `drop()` 壓力測試
- 跨執行緒 GC 收集正確性
- `Weak<T>` 升級正確性

### 6.3 Miri 測試

```bash
./miri-test.sh  # 必須通過記憶體安全檢查
```

### 6.4 Loom 測試 (未來)

```rust
#[test]
#[loom]
fn test_concurrent_ref_count() {
    // 並發 ref 操作正確性
}
```

---

## 7. 風險評估

| 風險 | 可能性 | 影響 | 緩和措施 |
|------|--------|------|----------|
| 效能回歸 | 中 | 中 | 提供效能基準測試 |
| 記憶體順序錯誤 | 中 | 高 | 全面 Miri 測試，仔細 SAFETY 註解 |
| 與現有 GC 基礎設施衝突 | 低 | 高 | 審查 `heap.rs` 中的同步邏輯 |

---

## 8. 參考資料

### 8.1 程式碼庫

- `crates/rudo-gc/src/ptr.rs` - Gc<T> 當前實現
- `crates/rudo-gc/src/heap.rs` - BiBOP 記憶體管理
- `crates/rudo-gc/src/gc.rs` - GC 收集算法

### 8.2 外部參考

- **dumpster**: `learn-projects/dumpster/dumpster/src/sync/mod.rs`
  - `unsafe impl<T> Send for Gc<T> where T: Trace + Send + Sync + ?Sized {}`
  - `unsafe impl<T> Sync for Gc<T> where T: Trace + Send + Sync + ?Sized {}`

- **ChezScheme**: `learn-projects/ChezScheme/`
  - `c/atomic.h` - 記憶體順序實作
  - `c/segment.h` - 層次化段表
  - `c/types.h` - BiBOP 結構
  - `c/thread.c` - 同步原語

### 8.3 論文

- *Don't Stop the BiBOP: Flexible and Efficient Storage Management for Dynamically Typed Languages* - Dybvig, Eby, Bruggeman (1994)

---

## 9. 版本歷史

| 版本 | 日期 | 描述 |
|------|------|------|
| 1.0 | 2026-01-27 | 初始草稿 |

---

## 10. 附錄

### A. ChezScheme BiBOP 架構摘要

```
Segment Table (層次化)
├── Level 3: S_segment_info[t3_idx] → t2table*
├── Level 2: t2table[t2_idx] → t1table*
└── Level 1: t1table[t1_idx] → seginfo*

seginfo (每段元數據)
├── marked_mask: octet*          // 標記位圖
├── generation: IGEN             // 代
├── space: ISPC                  // 空間類型
├── creator: thread_gc*          // 建立執行緒 (用於並行)
├── dirty_next/prev: seginfo*    // 髒段列表
└── old_space, use_marks: flags
```

### B. 記憶體順序速查表

| Ordering | 載入行為 | 儲存行為 | 使用場景 |
|----------|----------|----------|----------|
| Relaxed | 無 | 無 | 計數操作 |
| Acquire | 讀取同步 | 無 | 指標載入 |
| Release | 無 | 寫入同步 | 指標儲存 |
| AcqRel | 讀取同步 | 寫入同步 | dec_ref, dec_weak |
| SeqCst | 完全同步 | 完全同步 | GC 狀態轉換 |

---

**文件結束**
