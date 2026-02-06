# rudo-gc Metrics 改進計劃

**創建日期**: 2026-02-06
**作者**: AI Assistant
**版本**: 1.0

---

## 摘要

本文件詳細描述了 rudo-gc 的 metrics 系統改進計劃，目的是提升效能監控能力，使其達到可與 `gc-arena` 相當的水準，並支持更完善的 Profiling 需求。

---

## 1. 現狀分析

### 1.1 當前 metrics.rs 結構（82 行）

```rust
// crates/rudo-gc/src/metrics.rs

pub struct GcMetrics {
    pub duration: Duration,
    pub bytes_reclaimed: usize,
    pub bytes_surviving: usize,
    pub objects_reclaimed: usize,
    pub objects_surviving: usize,
    pub collection_type: CollectionType,
    pub total_collections: usize,
}

pub enum CollectionType {
    None = 0,
    Minor = 1,
    Major = 2,
    IncrementalMajor = 3,
}

// API
pub fn last_gc_metrics() -> GcMetrics
pub fn record_metrics(metrics: GcMetrics)
```

### 1.2 與 gc-arena 差距對比

| 功能 | rudo-gc | gc-arena | 差距 |
|------|---------|----------|------|
| Pacing 配置 | ❌ | ✅ `Pacing` struct | 高 |
| 分配債務計算 | ❌ | ✅ `allocation_debt()` | 高 |
| 標記/追蹤統計 | ❌ | ✅ `marked_gcs`, `traced_gcs` | 高 |
| 外部記憶體追蹤 | ❌ | ✅ `total_external_allocation()` | 中 |
| 週期統計 | ❌ | ✅ `allocated_gc_bytes` 等 | 中 |
| 動態配置 | ❌ | ✅ `set_pacing()` | 中 |
| 累積追蹤 | ❌ | ✅ `total_gcs`, `total_gc_bytes` | 中 |
| Phase 計時 | ❌ | 部分 | 低 |

### 1.3 現有能力評估

| 指標類型 | 狀態 | 說明 |
|---------|------|------|
| GC 持續時間 | ✅ | `duration` |
| 回收/存活位元組 | ✅ | `bytes_reclaimed`, `bytes_surviving` |
| 回收/存活物件數 | ✅ | `objects_reclaimed`, `objects_surviving` |
| 收集類型 | ✅ | `CollectionType` |
| 增量標記統計 | ⚠️ | `MarkStats` 存在但未整合到主要 API |
| 總收集次數 | ✅ | `total_collections` |

---

## 2. 改進目標

### 2.1 短期目標（P0）

1. **累積統計**：追蹤歷史趨勢
2. **增量標記整合**：將 `MarkStats` 統一到主要 API
3. **實時狀態查詢**：當前 heap 使用量

### 2.2 中期目標（P1）

1. **動態 Pacing 配置**
2. **Phase 級別計時**
3. **外部分配追蹤**

### 2.3 長期目標（P2）

1. **歷史環形緩衝區**
2. **自動匯出格式**
3. **與外部監控整合**

---

## 3. 詳細實作計劃

### Phase 1: 結構擴展

#### 3.1.1 新增 `GlobalMetrics` 結構

**檔案**: `crates/rudo-gc/src/metrics.rs`

```rust
/// Global GC statistics (thread-safe atomic access).
#[derive(Debug)]
pub struct GlobalMetrics {
    /// Currently live GC count.
    pub live_gc_count: AtomicUsize,
    /// Currently live bytes.
    pub live_gc_bytes: AtomicUsize,
    /// Total allocation count.
    pub total_allocations: AtomicUsize,
    /// Total free count.
    pub total_frees: AtomicUsize,
    /// External allocation bytes.
    pub external_bytes: AtomicUsize,
    /// Current heap size.
    pub current_heap_size: AtomicUsize,
    /// Allocation debt in nanoseconds.
    pub allocation_debt_nanos: AtomicU64,
}

const NS_PER_BYTE: u64 = 1000; // Calibration constant
```

#### 3.1.2 擴展 `GcMetrics` 結構

```rust
#[derive(Debug, Clone, Copy)]
pub struct GcMetrics {
    // === 現有欄位 ===
    /// Duration of the last collection.
    pub duration: Duration,
    /// Number of bytes reclaimed.
    pub bytes_reclaimed: usize,
    /// Number of bytes surviving.
    pub bytes_surviving: usize,
    /// Number of objects reclaimed.
    pub objects_reclaimed: usize,
    /// Number of objects surviving.
    pub objects_surviving: usize,
    /// Type of collection (Minor or Major).
    pub collection_type: CollectionType,
    /// Total collections since process start.
    pub total_collections: usize,

    // === 新增欄位 ===
    /// Number of bytes marked during this collection.
    pub bytes_marked: usize,
    /// Number of bytes traced during this collection.
    pub bytes_traced: usize,
    /// Number of bytes remembered during this collection.
    pub bytes_remembered: usize,
    /// Time spent in mark phase (sum of all slices for incremental).
    pub mark_time_ns: u64,
    /// Time spent in sweep phase.
    pub sweep_time_ns: u64,
    /// Whether fallback to STW occurred.
    pub fallback_occurred: bool,
    /// Number of slices executed (for incremental marking).
    pub slices_executed: usize,
    /// Number of dirty pages scanned.
    pub dirty_pages_scanned: usize,
}
```

#### 3.1.3 新增 CollectionPhase 計時

```rust
/// Timing for individual GC phases.
#[derive(Debug, Default)]
pub struct PhaseMetrics {
    pub mark_duration: Duration,
    pub sweep_duration: Duration,
    pub total_duration: Duration,
}
```

---

### Phase 2: API 擴展

#### 3.2.1 新增公開函數

```rust
/// Get global metrics (thread-safe).
#[must_use]
pub fn global_metrics() -> &'static GlobalMetrics;

/// Get current live bytes.
#[inline]
pub fn current_live_bytes() -> usize {
    global_metrics().live_gc_bytes.load(Ordering::Relaxed)
}

/// Get current allocation debt in bytes.
#[inline]
pub fn current_allocation_debt() -> usize {
    (global_metrics().allocation_debt_nanos.load(Ordering::Relaxed) / NS_PER_BYTE) as usize
}

/// Get total collections count.
#[inline]
pub fn total_collections() -> usize {
    global_metrics().total_allocations.load(Ordering::Relaxed)
}

/// Get external allocation bytes.
#[inline]
pub fn external_allocation_bytes() -> usize {
    global_metrics().external_bytes.load(Ordering::Relaxed)
}

/// Get current heap size.
#[inline]
pub fn current_heap_size() -> usize {
    global_metrics().current_heap_size.load(Ordering::Relaxed)
}

/// Check if incremental marking is currently active.
#[inline]
pub fn is_incremental_marking_active() -> bool {
    // Implementation in GlobalMarkState
    false // Placeholder
}
```

#### 3.2.2 配置 API

```rust
/// Get the current pacing configuration.
#[must_use]
pub fn get_pacing_config() -> PacingConfig;

/// Set the pacing configuration at runtime.
pub fn set_pacing_config(config: PacingConfig);
```

#### 3.2.3 歷史統計 API

```rust
/// Get recent GC history.
#[must_use]
pub fn gc_history() -> GcHistory;

/// Get average pause time over recent N collections.
#[must_use]
pub fn average_pause_time(n: usize) -> Duration;

/// Get maximum pause time over recent N collections.
#[must_use]
pub fn max_pause_time(n: usize) -> Duration;

/// Get total pause time across all collections.
#[must_use]
pub fn total_pause_time() -> Duration;
```

---

### Phase 3: Pacing 配置系統

#### 3.3.1 新建 `pacing.rs`

**檔案**: `crates/rudo-gc/src/pacing.rs`

```rust
/// Tuning parameters for incremental garbage collection.
#[derive(Debug, Clone, Copy)]
pub struct PacingConfig {
    /// Target maximum pause time in microseconds.
    pub target_max_pause_us: u64,
    /// Work units per byte for marking.
    pub mark_work_per_byte: f64,
    /// Work units per byte for tracing.
    pub trace_work_per_byte: f64,
    /// Work units per byte for sweeping.
    pub sweep_work_per_byte: f64,
    /// Minimum time slice for incremental work (microseconds).
    pub min_slice_us: u64,
    /// Maximum time slice for incremental work (microseconds).
    pub max_slice_us: u64,
    /// Maximum number of dirty pages before fallback.
    pub max_dirty_pages: usize,
}

impl PacingConfig {
    /// Default balanced configuration.
    pub const DEFAULT: Self = Self {
        target_max_pause_us: 1000,  // 1ms target
        mark_work_per_byte: 0.1,
        trace_work_per_byte: 0.4,
        sweep_work_per_byte: 0.1,
        min_slice_us: 100,
        max_slice_us: 5000,
        max_dirty_pages: 1024,
    };

    /// Low latency configuration for real-time systems.
    pub const LOW_LATENCY: Self = Self {
        target_max_pause_us: 100,   // 0.1ms target
        mark_work_per_byte: 0.05,
        trace_work_per_byte: 0.2,
        sweep_work_per_byte: 0.05,
        min_slice_us: 50,
        max_slice_us: 1000,
        max_dirty_pages: 256,
    };

    /// High throughput configuration.
    pub const THROUGHPUT: Self = Self {
        target_max_pause_us: 10000, // 10ms target
        mark_work_per_byte: 0.2,
        trace_work_per_byte: 0.8,
        sweep_work_per_byte: 0.2,
        min_slice_us: 500,
        max_slice_us: 50000,
        max_dirty_pages: 4096,
    };
}

/// Pacing state for a collection cycle.
#[derive(Debug)]
pub struct PacingState {
    /// Current allocation debt.
    pub debt: f64,
    /// Work performed this cycle.
    pub work_performed: f64,
    /// Target work for this cycle.
    pub target_work: f64,
}

impl PacingState {
    #[inline]
    pub fn should_collect(&self) -> bool {
        self.debt > 0.0 && self.work_performed < self.target_work
    }

    #[inline]
    pub fn add_debt(&mut self, bytes: usize, pacing: PacingConfig) {
        self.debt += bytes as f64;
        self.target_work += bytes as f64 * (pacing.mark_work_per_byte
            + pacing.trace_work_per_byte
            + pacing.sweep_work_per_byte);
    }

    #[inline]
    pub fn perform_work(&mut self, work: f64) {
        self.work_performed += work;
        self.debt = (self.debt - work).max(0.0);
    }
}
```

---

### Phase 4: 整合 MarkStats

#### 3.4.1 整合策略

**檔案**: `crates/rudo-gc/src/metrics.rs`

```rust
use crate::incremental::{GlobalMarkState, MarkStats};

impl GcMetrics {
    /// Create metrics from last collection with MarkStats integrated.
    #[inline]
    pub fn with_mark_stats(mut self, mark_stats: &MarkStats) -> Self {
        self.bytes_marked = mark_stats.objects_marked.load(Ordering::Relaxed) * AVG_OBJECT_SIZE;
        self.bytes_traced = self.bytes_marked; // Approximation
        self.mark_time_ns = mark_stats.mark_time_ns.load(Ordering::Relaxed);
        self.fallback_occurred = mark_stats.fallback_occurred.load(Ordering::Relaxed);
        self.slices_executed = mark_stats.slices_executed.load(Ordering::Relaxed);
        self.dirty_pages_scanned = mark_stats.dirty_pages_scanned.load(Ordering::Relaxed);
        self
    }
}

/// Constants for metrics calculations.
const AVG_OBJECT_SIZE: usize = 32; // Calibration: average object size in bytes
```

#### 3.4.2 在收集循環中整合

```rust
// In GlobalMarkState or equivalent
impl GlobalMarkState {
    pub fn get_mark_stats(&self) -> MarkStats {
        MarkStats {
            objects_marked: self.objects_marked.clone(),
            dirty_pages_scanned: self.dirty_pages_scanned.clone(),
            slices_executed: self.slices_executed.clone(),
            mark_time_ns: self.mark_time_ns.clone(),
            fallback_occurred: self.fallback_occurred.clone(),
            fallback_reason: self.fallback_reason.clone(),
        }
    }
}
```

---

### Phase 5: 實現債務計算

#### 3.5.1 分配債務追蹤

```rust
/// Track allocation debt for incremental collection.
#[derive(Debug)]
pub struct AllocationDebt {
    /// Current debt in bytes.
    debt_bytes: AtomicUsize,
    /// Work performed in current cycle.
    work_performed: AtomicUsize,
}

impl AllocationDebt {
    #[inline]
    pub fn add_debt(&self, bytes: usize) {
        self.debt_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline]
    pub fn perform_work(&self, work: usize) {
        self.work_performed.fetch_add(work, Ordering::Relaxed);
    }

    #[inline]
    pub fn remaining_debt(&self) -> usize {
        self.debt_bytes.load(Ordering::Relaxed).saturating_sub(
            self.work_performed.load(Ordering::Relaxed)
        )
    }

    #[inline]
    pub fn reset(&self) {
        let remaining = self.remaining_debt();
        self.debt_bytes.store(remaining, Ordering::Relaxed);
        self.work_performed.store(0, Ordering::Relaxed);
    }

    #[inline]
    pub fn debt_ratio(&self) -> f64 {
        let debt = self.debt_bytes.load(Ordering::Relaxed) as f64;
        let work = self.work_performed.load(Ordering::Relaxed) as f64;
        if work == 0.0 {
            1.0
        } else {
            (debt / work).clamp(0.0, 1.0)
        }
    }
}
```

---

### Phase 6: 歷史記錄（可選）

#### 3.6.1 環形緩衝區

```rust
/// History of recent GC collections.
pub struct GcHistory {
    /// Ring buffer of recent metrics.
    buffer: [GcMetrics; N_HISTORY],
    /// Current write index.
    index: AtomicUsize,
    /// Total collections recorded.
    total: AtomicUsize,
}

impl GcHistory {
    pub const N_HISTORY: usize = 64;

    #[inline]
    pub fn record(&self, metrics: GcMetrics) {
        let idx = self.index.fetch_add(1, Ordering::Relaxed) % Self::N_HISTORY;
        self.buffer[idx] = metrics;
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn recent(&self, n: usize) -> impl Iterator<Item = GcMetrics> + '_
    {
        let total = self.total();
        let start = total.saturating_sub(n).max(total.saturating_sub(Self::N_HISTORY));
        (start..total).map(move |i| self.buffer[i % Self::N_HISTORY])
    }

    #[inline]
    pub fn average_pause_time(&self) -> Duration {
        let recent: Vec<_> = self.recent(10).collect();
        if recent.is_empty() {
            Duration::ZERO
        } else {
            let total_ns: u128 = recent.iter().map(|m| m.duration.as_nanos()).sum();
            Duration::from_nanos((total_ns / recent.len() as u128) as u64)
        }
    }

    #[inline]
    pub fn max_pause_time(&self) -> Duration {
        self.recent(10)
            .map(|m| m.duration)
            .max()
            .unwrap_or(Duration::ZERO)
    }

    #[inline]
    pub fn total(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn total_bytes_reclaimed(&self) -> usize {
        self.recent(100).map(|m| m.bytes_reclaimed).sum()
    }
}
```

---

## 4. API 使用範例

```rust
use rudo_gc::{Gc, GcArena, metrics::PacingConfig};

fn example() {
    // === 基本監控 ===
    println!("Live bytes: {}", rudo_gc::metrics::current_live_bytes());
    println!("Allocation debt: {} bytes", rudo_gc::metrics::current_allocation_debt());
    println!("Total collections: {}", rudo_gc::metrics::total_collections());
    println!("External allocation: {} bytes", rudo_gc::metrics::external_allocation_bytes());
    println!("Heap size: {} bytes", rudo_gc::metrics::current_heap_size());

    // === 最後一次 GC 指標 ===
    let metrics = rudo_gc::metrics::last_gc_metrics();
    println!("GC duration: {:?}", metrics.duration);
    println!("Bytes reclaimed: {}", metrics.bytes_reclaimed);
    println!("Objects reclaimed: {}", metrics.objects_reclaimed);
    println!("Collection type: {:?}", metrics.collection_type);
    println!("Fallback occurred: {}", metrics.fallback_occurred);
    println!("Slices executed: {}", metrics.slices_executed);

    // === 動態調整 Pacing ===
    rudo_gc::metrics::set_pacing_config(PacingConfig::LOW_LATENCY);

    // === 歷史分析 ===
    let history = rudo_gc::metrics::gc_history();
    println!("Avg pause time: {:?}", history.average_pause_time());
    println!("Max pause time: {:?}", history.max_pause_time());
    println!("Total reclaimed: {} bytes", history.total_bytes_reclaimed());
}

// 基準測試範例
fn benchmark() {
    let arena = GcArena::new();

    for _ in 0..1000 {
        arena.mutate(|mc| {
            // Allocate and use GC objects
        });
    }

    // 獲取最終指標
    let metrics = rudo_gc::metrics::last_gc_metrics();
    println!("Throughput: {} bytes/collection",
             metrics.bytes_reclaimed / metrics.total_collections.max(1));
}
```

---

## 5. 實作順序

| 順序 | 任務 | 檔案變更 | 複雜度 | 優先級 |
|------|------|----------|--------|--------|
| 1 | 新增 GlobalMetrics 結構 | `metrics.rs` | 中 | P0 |
| 2 | 擴展 GcMetrics 欄位 | `metrics.rs` | 低 | P0 |
| 3 | 實現實時查詢 API | `metrics.rs` | 低 | P0 |
| 4 | 建立 pacing.rs | 新建 `pacing.rs` | 中 | P1 |
| 5 | 整合 MarkStats | `metrics.rs` + `incremental.rs` | 中 | P1 |
| 6 | 實現債務計算 | `metrics.rs` | 高 | P1 |
| 7 | 可選：歷史記錄 | `metrics.rs` | 高 | P2 |

---

## 6. Profiling 整合

### 6.1 與 Criterion 整合

```rust
// benches/gc_metrics.rs

use criterion::{criterion_group, criterion_main, Criterion};
use rudo_gc::{GcArena, metrics};

fn allocation_throughput(c: &mut Criterion) {
    let arena = GcArena::new();

    c.bench_function("gc_allocation_1k", |b| {
        b.iter(|| {
            arena.mutate(|mc| {
                for i in 0..1000 {
                    mc.allocate(MyStruct { value: i });
                }
            });
        });
    });
}

fn gc_metrics_benchmark(c: &mut Criterion) {
    let arena = GcArena::new();

    // 預熱
    for _ in 0..100 {
        arena.mutate(|mc| mc.allocate(MyStruct { value: 0 }));
    }

    c.bench_function("gc_last_metrics", |b| {
        b.iter(|| {
            metrics::last_gc_metrics();
        });
    });
}

criterion_group!(benches, allocation_throughput, gc_metrics_benchmark);
criterion_main!(benames);
```

### 6.2 與 DHat 整合

```rust
#[cfg(feature = "profiling")]
use dhat::{Dhat, DhatAlloc};

#[global_allocator]
#[cfg(feature = "profiling")]
static ALLOC: DhatAlloc = DhatAlloc;

#[test]
fn test_allocation_memory() {
    #[cfg(feature = "profiling")]
    let _dhat = Dhat::start_profiling();

    let arena = GcArena::new();
    for _ in 0..10000 {
        arena.mutate(|mc| {
            mc.allocate(MyStruct { value: 0 });
        });
    }
}
```

### 6.3 與 Flamegraph 整合

```bash
# 生成 GC 火焰圖
cargo flamegraph --bin benchmark -- --bench

# 針對特定場景
cargo flamegraph --freq 999 --bin benchmark --features profiling -- --test allocation
```

---

## 7. 風險與注意事項

### 7.1 執行緒安全

- 使用 `AtomicUsize` 而非 `Cell` 確保跨執行緒安全
- 所有公開 API 需標記為 `#[inline]`
- 使用適當的 `Ordering`（Relaxed 通常足夠）

### 7.2 效能開銷

- 原子操作有輕微開銷（約 1-2 個 CPU 週期）
- 建議為熱路徑指標添加 `#[inline]`
- 可考慮使用 `AtomicU64` 替代 `AtomicUsize` 避免跨平台大小問題

### 7.3 向後相容

- 現有 API 保持不變
- 新增欄位使用預設值（0）
- `#[non_exhaustive]` 可選

### 7.4 記憶體佔用

- 歷史緩衝區設定上限（建議 64 項）
- GlobalMetrics 僅存儲原子變數

---

## 8. 測試計劃

### 8.1 單元測試

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_metrics_defaults() {
        let metrics = GcMetrics::new();
        assert_eq!(metrics.duration, Duration::ZERO);
        assert_eq!(metrics.bytes_reclaimed, 0);
        assert_eq!(metrics.slices_executed, 0);
    }

    #[test]
    fn test_allocation_debt() {
        let debt = AllocationDebt::new();
        debt.add_debt(1000);
        assert_eq!(debt.remaining_debt(), 1000);
        debt.perform_work(500);
        assert_eq!(debt.remaining_debt(), 500);
    }

    #[test]
    fn test_pacing_state() {
        let pacing = PacingConfig::DEFAULT;
        let mut state = PacingState::new();
        state.add_debt(1000, pacing);
        assert!(state.should_collect());
        state.perform_work(100.0);
        assert!(state.debt_ratio() < 1.0);
    }
}
```

### 8.2 整合測試

```rust
#[test]
fn test_metrics_round_trip() {
    let arena = GcArena::new();

    // Allocate some objects
    arena.mutate(|mc| {
        for i in 0..100 {
            mc.allocate(Data { value: i });
        }
    });

    // Force collection
    arena.collect();

    // Verify metrics
    let metrics = last_gc_metrics();
    assert!(metrics.bytes_reclaimed > 0 || metrics.bytes_surviving > 0);
    assert!(metrics.duration > Duration::ZERO);
}
```

---

## 9. 文件更新

### 9.1 需要更新的文件

1. `crates/rudo-gc/src/lib.rs` - 新增 public re-exports
2. `crates/rudo-gc/src/metrics.rs` - 主要實作
3. `docs/` - API 文件
4. `examples/` - 使用範例

### 9.2 新增示例

```rust
// examples/metrics_demo.rs
//
// Demonstrates how to use the metrics API for monitoring GC performance.
```

---

## 10. 預計工作時數

| Phase | 任務 | 估計時數 |
|-------|------|----------|
| 1 | 結構擴展 | 4-6 小時 |
| 2 | API 擴展 | 2-4 小時 |
| 3 | Pacing 系統 | 6-8 小時 |
| 4 | MarkStats 整合 | 4-6 小時 |
| 5 | 債務計算 | 4-6 小時 |
| 6 | 歷史記錄 | 4-8 小時 |
| - | 測試與除錯 | 4-8 小時 |
| **合計** | | **28-46 小時** |

---

## 11. 參考資料

### 11.1 內部參考

- `crates/rudo-gc/src/metrics.rs` - 現有實作
- `crates/rudo-gc/src/incremental.rs` - MarkStats 定義
- `learn-projects/gc-arena/src/metrics.rs` - gc-arena 的實現

### 11.2 外部參考

- [Rust Atomic Types](https://doc.rust-lang.org/std/sync/atomic/)
- [Criterion Benchmarks](https://bheisler.github.io/criterion.rs/book/)
- [DHat Heap Profiler](https://docs.rs/dhat/latest/dhat/)

---

## 12. 變更歷史

| 版本 | 日期 | 變更 |
|------|------|------|
| 1.0 | 2026-02-06 | 初始版本 |

