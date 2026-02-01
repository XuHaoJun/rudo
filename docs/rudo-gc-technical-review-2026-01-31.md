# rudo-gc 技術規格評審報告

**評審日期**: 2026-01-31  
**評審文件**: HandleScope 技術規格 v1.0, rudo-gc 核心實作  
**評審者**: 
- Rust Leadership Council (模擬)
- R. Kent Dybvig (Chez Scheme 作者，模擬)  

**評審對象**:
- `crates/rudo-gc` — 核心 GC 實作
- `docs/handlescope-spec.md` — HandleScope 提案
- `learn-projects/v8` — V8 參考實作

---

## 執行摘要

`rudo-gc` 是一個 ambitious 的專案，試圖為 Rust 生態系統提供類似 V8/Go 體驗的垃圾收集器。此次評審聚焦於：

1. **現有實作的 Soundness 問題**
2. **HandleScope 提案的技術可行性**
3. **與 V8 架構的比較分析**
4. **前瞻性建議**

**結論**: HandleScope 提案是一個**正確方向**的架構改進，能從根本解決 Conservative Stack Scanning 的 soundness 問題。但實作細節需要進一步精煉，特別是與現有架構的整合路徑。

---

## Part I: Rust Leadership Council 評審

### 1. 現有架構的 Soundness 問題 ⚠️

#### 1.1 Conservative Stack Scanning 的根本缺陷

**問題嚴重程度**: 🔴 Critical

現有的 `spill_registers_and_scan()` 實作（`stack.rs:137-230`）存在以下根本性問題：

```rust
// stack.rs:143-161 - x86_64 callee-saved registers
std::arch::asm!(
    "mov {0}, rbx",
    "mov {1}, rbp",
    "mov {2}, r12",
    // ...
);
```

**Council 評估**:

| 風險維度 | 評估 | 說明 |
|---------|------|------|
| **Register Coverage** | 🟡 Partial | 僅覆蓋 callee-saved，LLVM 可能使用 caller-saved |
| **Vector Registers** | 🔴 Missing | AVX/SSE registers 完全未掃描 |
| **Provenance** | 🔴 UB | `ptr as usize` 轉換在 Strict Provenance 下是 undefined |
| **Interior Pointers** | 🔴 UAF | Small objects 不支援 interior pointer（見下文） |

#### 1.2 Interior Pointer 漏洞分析

**程式碼位置**: `heap.rs:find_gc_box_from_ptr()` 

```rust
// heap.rs - 問題程式碼
} else if offset_to_use % block_size_to_use != 0 {
    // For small objects, we still require them to point to the start
    return None;  // ← CRITICAL: This causes UAF
}
```

**攻擊場景**:

```rust
struct Node { a: u64, b: u64 }  // Size: 16 bytes

fn vulnerable() {
    let node = Gc::new(Node { a: 1, b: 2 });
    let ref_b = &node.b;  // Interior pointer at offset +8
    drop(node);           // Stack only contains ref_b
    
    // GC runs:
    // - Scanner finds ref_b (interior pointer)
    // - find_gc_box_from_ptr calculates offset=8, block_size=16
    // - 8 % 16 != 0 → returns None
    // - Node is collected
    // - ref_b is now dangling → UAF!
    
    println!("{}", *ref_b);  // 💥 Use-After-Free
}
```

**LLVM Optimization Context**:
在 Release mode 下，LLVM 的 **Scalar Replacement of Aggregates (SROA)** 非常激進地將 struct 拆解。上述場景在實際編譯中極易發生。

#### 1.3 Pointer Provenance 問題

**現況**:

```rust
// 在 scan.rs 中大量使用
let addr = ptr as usize;
let ptr_back = addr as *const u8;
```

**Council 立場**:
Rust 正朝向 **Strict Provenance** 模型發展（[RFC 3559](https://rust-lang.github.io/rfcs/3559-rust-has-provenance.html)）。目前的實作：

1. 在 **Miri** 下需要特殊 workaround（`#[cfg(miri)]` 分支）
2. 在 **CHERI** 架構硬體上將完全失效
3. 未來 Rust 版本可能將此視為 UB

**評審建議**: 這個問題無法在 Conservative Scanning 架構下徹底解決，只有轉向 **Exact Roots** 才能從根本消除。

---

### 2. HandleScope 提案評估 ✅

#### 2.1 正確方向

HandleScope 提案（`handlescope-spec.md`）正確識別了問題並提出了合理的解決方案。核心設計參考 V8 是明智的選擇。

**V8 HandleScope 架構對照**:

| V8 組件 | rudo-gc 對應 | 評估 |
|---------|-------------|------|
| `HandleScopeData` | `HandleScopeData` | ✅ 設計一致 |
| `LocalHandles` | 擴展 `ThreadControlBlock` | ✅ 架構合理 |
| `LocalHandleScope` | `HandleScope<'a>` | ✅ RAII 符合 Rust 習慣 |
| `Handle<T>` | `Handle<T>` | ⚠️ 需精煉（見下文） |

#### 2.2 設計優點

1. **Exact Root Tracing**: 完全消除掃描遺漏的風險
2. **RAII 管理**: 利用 Rust `Drop` trait 自動處理 scope 邊界
3. **漸進式遷移**: Feature flag 設計允許逐步引入
4. **V8 實戰驗證**: Handle 機制在 Chrome 數十億用戶環境驗證

#### 2.3 設計需改進之處

**Issue 1: Handle<T> 的生命週期綁定**

提案中的 `Handle<T>` 設計：

```rust
pub struct Handle<T: Trace> {
    ptr: *const GcBox<T>,  // ← 無生命週期約束
}
```

**問題**: 這允許 Handle 逃逸出 HandleScope，違反設計意圖。

**建議修正**:

```rust
pub struct Handle<'scope, T: Trace> {
    ptr: *const GcBox<T>,
    _scope: PhantomData<&'scope ()>,  // 綁定至 scope 生命週期
}

impl<'scope> HandleScope<'scope> {
    pub fn create_handle<T: Trace>(&'scope self, gc: &Gc<T>) 
        -> Handle<'scope, T> 
    {
        // ...
    }
}
```

這確保 Handle 無法逃逸出創建它的 scope。

**Issue 2: `Handle::new()` 依賴 `HandleScope::current()`**

```rust
impl<T: Trace> Handle<T> {
    pub fn new(gc: &Gc<T>) -> Self {
        let mut scope = HandleScope::current();  // ← 隱式全域狀態
        // ...
    }
}
```

**問題**: 
- 隱式依賴 thread-local 狀態
- 如果沒有 active scope 會 panic
- 不符合 Rust explicit is better 的哲學

**建議修正**:

```rust
// 顯式傳遞 scope
let handle = scope.create_handle(&gc);

// 或者使用 macro 減少 boilerplate
let handle = handle!(scope, &gc);
```

**Issue 3: `iterate_handles` 的安全性**

```rust
pub fn iterate_handles(&self, visitor: &mut GcVisitor) {
    for block_ptr in &self.handle_blocks {
        let block = unsafe { &*block_ptr.as_ptr() };
        // ...
    }
}
```

**問題**: 遍歷時沒有處理 concurrent modification 的情況。

**建議**: 在 GC 期間確保所有執行緒已停止（STW），或使用適當的同步機制。

---

### 3. API Soundness 審查

#### 3.1 Tokio 整合的安全性 🔴

**現狀分析**:

README 要求使用者手動呼叫 `root_guard()`:

```rust
let gc = Gc::new(42);
tokio::spawn(async move {
    let _guard = gc.root_guard();  // 使用者必須記得！
    // ...
});
```

**Council 評估**: 這是 **Unsound API**。在 Safe Rust 中忘記一行程式碼就會導致 UAF，這違反 Rust 的安全承諾。

**HandleScope 如何改善**:

```rust
// HandleScope 版本
let gc = Gc::new(42);
let handle = scope.create_handle(&gc);

tokio::spawn(async move {
    // handle 不能跨 scope 傳遞（編譯錯誤）
    // 或需要 EscapeHandle 機制
});
```

**建議方案**:

1. **短期**: 提供 `spawn_with_gc!()` macro 強制正確使用
2. **長期**: 設計類似 V8 `EscapeableHandleScope` 的機制

#### 3.2 Send/Sync 實作審查

```rust
// heap.rs:56-57
unsafe impl Send for ThreadControlBlock {}
unsafe impl Sync for ThreadControlBlock {}
```

**評估**: 
- 這需要極其謹慎的審查
- 必須確保所有內部可變狀態都有適當的同步
- 建議添加更詳細的 SAFETY 文件

---

## Part II: R. Kent Dybvig 評審

*作為 Chez Scheme 垃圾收集器的設計者，以下是我對 rudo-gc 的技術觀點。*

### 1. 記憶體佈局評估 (BiBOP) ✅

`rudo-gc` 採用的 **Big Bag of Pages (BiBOP)** 架構與 Chez Scheme 的設計有相似之處：

**優點**:
- **O(1) 分配**: Bump pointer allocation 非常高效
- **Size-class 分離**: 減少外部碎片化
- **Page 層級元資料**: 利於快速物件識別

**Chez Scheme 對比**:

| 特性 | Chez Scheme | rudo-gc | 評估 |
|------|-------------|---------|------|
| 分配策略 | Bump pointer + copying | Bump pointer + mark-sweep | ✅ 合理 |
| 世代維護 | Generational | Generational (Young/Old) | ✅ 一致 |
| Interior Pointers | 完整支援 | 🔴 僅 Large Objects | 需修復 |
| Stack Scanning | Precise (continuation) | Conservative | ✅ HandleScope 改進 |

### 2. Mark-Sweep vs. Copying GC

**設計決策評估**:

rudo-gc 選擇 **Non-moving Mark-Sweep** 而非 Copying GC：

```
優點:
+ 保持指標穩定性（&T 不會失效）
+ 與 Rust 借用規則相容
+ 實作較簡單

缺點:
- 記憶體碎片化
- 無法利用 locality 優化
```

**Dybvig 評論**: 這是正確的取捨。在 Rust 中實作 Moving GC 需要解決 **pointer update** 問題，這與 Rust 的引用語義衝突。Non-moving 是務實的選擇。

### 3. HandleScope 與 Chez Scheme Continuation 的比較

Chez Scheme 使用 **precise stack walking** via continuations：

```scheme
; Chez Scheme 的 continuation 保存完整的 stack frame
(call/cc (lambda (k) ...))
```

這與 HandleScope 的精確根追蹤有異曲同工之妙：

- **Chez**: Continuation 保存所有活躍變數
- **V8/rudo-gc HandleScope**: Handle blocks 保存所有 GC 指標

**關鍵差異**:

| 維度 | Chez Continuation | HandleScope |
|------|------------------|-------------|
| 粒度 | Per-frame | Per-scope |
| 成本 | 較高（完整快照）| 較低（僅指標）|
| 控制流 | 支援 first-class continuation | 不支援 |

### 4. GC Scheduling 建議

**現況**:

```rust
// heap.rs - 觸發條件
pub fn default_collect_condition(info: &CollectInfo) -> bool {
    // 基於分配壓力的啟發式
}
```

**Chez Scheme 實踐**:

1. **Generation-based trigger**: 年輕代固定大小後觸發 minor GC
2. **Promotion threshold**: 存活超過 N 次 minor GC 的物件晉升
3. **Major GC pacing**: 基於 heap growth rate 預測

**建議**: 考慮加入 **GC pacing** 機制，根據 mutator 分配速率動態調整 GC 頻率。

### 5. Interior Pointer 修復建議

**Chez Scheme 方法**:

```
對每個 potential pointer P:
1. 找到包含 P 的 page
2. 從 page header 獲取 object size class
3. 計算 P 所在的 object 起始位置: 
   obj_start = page_base + (offset / obj_size) * obj_size
4. 驗證 obj_start 是 allocated object
5. 標記 obj_start
```

**rudo-gc 修復**:

```rust
fn find_gc_box_from_ptr_interior(
    heap: &LocalHeap,
    ptr: *const u8,
) -> Option<NonNull<GcBox<()>>> {
    let header = ptr_to_page_header(ptr)?;
    let page_base = header.as_ptr() as usize + PAGE_HEADER_SIZE;
    let offset = ptr as usize - page_base;
    let block_size = (*header.as_ptr()).block_size as usize;
    
    // Interior pointer support: round down to object start
    let object_index = offset / block_size;
    
    // Validate: check if this slot is marked as allocated
    if !(*header.as_ptr()).is_allocated(object_index) {
        return None;
    }
    
    let object_ptr = page_base + object_index * block_size;
    Some(NonNull::new_unchecked(object_ptr as *mut GcBox<()>))
}
```

---

## Part III: 綜合建議

### 1. 短期修復（High Priority）

| ID | 項目 | 優先級 | 預估工時 |
|----|------|--------|----------|
| F1 | Interior Pointer 支援 | P0 | 2 天 |
| F2 | `root_guard()` 強制化 macro | P0 | 1 天 |
| F3 | SAFETY 文件補充 | P1 | 1 天 |

### 2. HandleScope 實作順序

```
Phase 1 (v0.6.0): 實驗性引入
├── HandleScopeData 結構
├── HandleBlock 分配器
├── HandleScope RAII
└── Feature flag: handle-scope

Phase 2 (v0.7.0): 預設啟用
├── 整合 GC root 收集
├── 效能測試
└── 文件與遷移指南

Phase 3 (v1.0.0): 移除 Conservative Scanning
├── 移除 stack.rs
├── Full Exact Roots
└── Provenance 問題解決
```

### 3. 架構演進路線圖

```
現狀 (v0.5.x)
╔══════════════════════════════════════════╗
║  User Code                               ║
║  Gc::new(x) → Conservative Stack Scan    ║
╠══════════════════════════════════════════╣
║  BiBOP Heap + TLAB + Mark-Sweep         ║
╚══════════════════════════════════════════╝
                    │
                    ▼ Phase 1-2
╔══════════════════════════════════════════╗
║  User Code                               ║
║  HandleScope { handle!(gc) }             ║
║  ─────────────────────────────────────   ║
║  Fallback: Conservative Scan (optional)  ║
╠══════════════════════════════════════════╣
║  BiBOP Heap + TLAB + Mark-Sweep         ║
╚══════════════════════════════════════════╝
                    │
                    ▼ Phase 3 (v1.0)
╔══════════════════════════════════════════╗
║  User Code                               ║
║  HandleScope { handle!(gc) }             ║
║  ─────────────────────────────────────   ║
║  Exact Roots (No Stack Scanning)         ║
╠══════════════════════════════════════════╣
║  BiBOP Heap + TLAB + Mark-Sweep         ║
╚══════════════════════════════════════════╝
```

### 4. 與競品比較

| 特性 | rudo-gc (目標) | gc-arena | dumpster | bdwgc |
|------|---------------|----------|----------|-------|
| **Soundness** | ✅ (with HandleScope) | ✅ | ✅ | ⚠️ |
| **Ergonomics** | ✅ | ❌ (closure-based) | ✅ | ✅ |
| **Performance** | ✅ (BiBOP) | ✅ | ⚠️ (RefCount) | ✅ |
| **Multi-thread** | ✅ | ❌ | ✅ | ✅ |
| **Rust-native** | ✅ | ✅ | ✅ | ❌ (C FFI) |

---

## 結論

### Rust Leadership Council 總結

`rudo-gc` 有成為 Rust 生態系優秀 GC 的潛力，但目前存在 **critical soundness issues**。HandleScope 提案是正確方向，建議：

1. **立即**: 修復 Interior Pointer UAF 問題
2. **短期**: 實作 HandleScope Phase 1
3. **長期**: 完全移除 Conservative Stack Scanning

### R. Kent Dybvig 總結

從 GC 理論角度，`rudo-gc` 的設計決策大多合理：

- BiBOP 架構高效務實
- Non-moving 策略與 Rust 相容
- HandleScope 類似 Chez 的 precise root tracking

建議關注：

1. Interior Pointer 支援是必須的
2. GC pacing 可提升回應性
3. 世代策略可進一步優化

---

## 附錄：V8 HandleScope 原始碼索引

| 檔案 | 關鍵內容 |
|------|----------|
| `src/handles/local-handles.h:19-42` | `LocalHandles` 類別定義 |
| `src/handles/local-handles.h:44-89` | `LocalHandleScope` RAII |
| `src/handles/handles.h:263-347` | `HandleScope` 核心實作 |
| `src/handles/handles.h:378-382` | Direct Handle + CSS 整合 |
| `src/heap/local-heap.h:50-76` | Thread-local heap binding |

---

*此評審報告由 Gemini 基於 Rust Leadership Council 與 R. Kent Dybvig 的技術視角模擬生成。*
