# RLC 設計回饋驗證報告

> 生成日期：2026-01-29
> 目的：驗證 RLC 回饋文件中的四個關鍵疑慮

---

## 摘要

本文件驗證了來自「Rust Leadership Council (RLC)」虛擬回饋中提出的四個關鍵疑慮。經過原始碼檢查後，發現：

| 疑慮 | RLC 判斷 | 驗證結果 |
|------|----------|----------|
| GcBox Header 40 bytes | ✅ 有效建議 | **正確** - 需要優化 |
| 保守堆疊掃描限制 | ✅ 有效擔憂 | **正確但有緩解機制** |
| 安全點可靠性 | ⚠️ 嚴重風險 | **正確且嚴重** - 設計漏寫 |
| 原子操作開銷 | ✅ 有效建議 | **合理建議** |

**最關鍵發現**：協作式安全點的無限迴圈風險是設計文件中**完全沒有提及的嚴重缺陷**。

---

## 1. GcBox Header 開銷

### RLC 原始回饋

```
GcBox<T> header 高達 40 bytes（64-bit 系統）。
對於像 Gc<i32> 或 Gc<Node> 這樣的小物件，Header 比資料還大。

建議：考慮將 drop_fn 和 trace_fn 移入靜態 VTable。
```

### 原始碼確認

**位置**: `crates/rudo-gc/src/ptr.rs:22-37`

```rust
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: AtomicUsize,           // 8 bytes
    weak_count: AtomicUsize,          // 8 bytes
    drop_fn: unsafe fn(*mut u8),      // 8 bytes
    trace_fn: unsafe fn(*const u8, &mut GcVisitor), // 8 bytes
    is_dropping: AtomicUsize,         // 8 bytes
    value: T,
}
// 總計：40 bytes header + T 的內容
```

### 驗證結果

| 項目 | 結果 |
|------|------|
| RLC 計算是否正確？ | ✅ 正確（40 bytes header） |
| 設計文件是否提及？ | ✅ 有提及（第 9 點） |
| VTable 優化是否已實現？ | ❌ 否 |
| 評估 | **RLC 建議有效，這是一個合理的優化方向** |

---

## 2. 保守堆疊掃描風險

### RLC 原始回饋

```
保守堆疊掃描的風險：
1. 誤判風險：整數可能被誤識別為指標，導致 OOM
2. 無法實現移動 GC（Compaction/Moving）
3. LLVM 優化可能導致 UB
```

### 原始碼確認

**位置**: `crates/rudo-gc/src/stack.rs:136-230`

```rust
pub unsafe fn spill_registers_and_scan<F>(mut scan_fn: F)
where
    F: FnMut(usize, usize, bool),
{
    // x86_64: 溢位 callee-saved registers
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    unsafe {
        std::arch::asm!(
            "mov {0}, rbx",
            "mov {1}, rbp",
            "mov {2}, r12",
            // ...
        );
    }
    // 掃描整個 stack
    while current < bounds.bottom {
        let potential_ptr = unsafe { std::ptr::read_volatile(current as *const usize) };
        scan_fn(potential_ptr, current, false);
        current += std::mem::size_of::<usize>();
    }
}
```

### 設計文件中漏寫的緩解機制

| 機制 | 位置 | 說明 | 設計文件提及？ |
|------|------|------|----------------|
| `clear_registers()` | `stack.rs:237-285` | 清除分配器的 registers | ❌ 漏寫 |
| `MASK` 機制 | `heap.rs:839-841` | 頁面地址 XOR MASK 過濾 | ❌ 漏寫 |
| Stack conflict 檢測 | `heap.rs:886-912` | 分配時檢測 False Roots | ❌ 漏寫 |

### Shadow Stack 檢查

| 問題 | 答案 |
|------|------|
| rudo-gc 是否有 Shadow Stack？ | ❌ **沒有** |
| 是否計劃實現？ | ❌ 沒有提及 |

### 驗證結果

| 項目 | 結果 |
|------|------|
| RLC 計算是否正確？ | ✅ 正確 - 確實是保守掃描 |
| 無法實現 Compaction？ | ✅ 正確 |
| 設計文件是否提及風險？ | ⚠️ 部分提及但不完全 |
| 緩解機制是否在設計文件中？ | ❌ **漏寫** - 多個機制未記錄 |
| 評估 | **RLC 擔憂正確，但已有部分緩解機制未記錄** |

---

## 3. 協作式安全點可靠性

### RLC 原始回饋

```
協作式安全點的可靠性問題：

如果執行緒進入緊湊的計算迴圈（沒有分配，也沒有函數呼叫），
它可能永遠不會檢查 GC_REQUESTED。

這會導致所有其他執行緒在 enter_rendezvous 處卡死
（Stop-the-World 變成 Stop-Forever）。
```

### 原始碼確認

**位置**: `crates/rudo-gc/src/heap.rs:183-190`

```rust
pub fn check_safepoint() {
    // 只有在分配時才會呼叫這個函數
    if GC_REQUESTED.load(Ordering::Relaxed) && !crate::gc::is_collecting() {
        enter_rendezvous();
    }
}
```

**關鍵問題**：`check_safepoint()` 只在**分配時**呼叫。

### 已暴露的緩解 API

**位置**: `crates/rudo-gc/src/lib.rs:84` 和 `gc.rs:183-203`

```rust
// 已導出給使用者
pub fn safepoint() {
    crate::heap::check_safepoint();
}

/// 手動檢查 GC 請求並阻塞直到處理完成。
///
/// 此函數應該在長期執行的迴圈中呼叫（不進行分配的情況下），
/// 以確保執行緒能及時響應 GC 請求。
pub fn safepoint() { ... }
```

### 驗證結果

| 項目 | 結果 |
|------|------|
| RLC 擔憂是否有效？ | ✅ **完全正確** - 沒有分配就會 Stop-Forever |
| `safepoint()` 是否存在？ | ✅ 存在 |
| 設計文件是否提及 `safepoint()`？ | ❌ **嚴重漏寫** |
| 設計文件是否提及無限迴圈風險？ | ❌ **嚴重漏寫** |
| `safepoint()` 是否緩解問題？ | ⚠️ **部分緩解** - 需要使用者手動呼叫 |
| 評估 | **RLC 批評完全正確，這是設計文件嚴重的遺漏** |

### 未解決的問題

| 問題 | 狀態 |
|------|------|
| 第三方庫（tokio, rayon）不使用 `safepoint()` | ❌ 未解決 |
| async/await 狀態機中的 root 追蹤 | ❌ 未解決 |
| 編譯器輔助安全點（compiler-assisted safepoints） | ❌ 未規劃 |

---

## 4. 原子操作開銷

### RLC 原始回饋

```
Bitmap 操作使用了 AtomicU64。
雖然 fetch_or 很快，但在標記階段，大量的原子操作會導致
Cache Coherence Traffic 暴增。

建議：考慮在 TLAB 或本地緩衝區進行非原子標記。
```

### 原始碼確認

**位置**: `crates/rudo-gc/src/heap.rs:508-513`

```rust
pub fn set_mark(&mut self, index: usize) -> bool {
    let word = index / 64;
    let bit = index % 64;
    let mask = 1u64 << bit;
    let old = self.mark_bitmap[word].fetch_or(mask, Ordering::AcqRel);
    (old & mask) == 0
}
```

### 驗證結果

| 項目 | 結果 |
|------|------|
| RLC 分析是否正確？ | ✅ 正確 - 確實使用原子操作 |
| 設計文件是否提及此優化？ | ❌ 漏寫 |
| TLAB 緩衝方案是否已實現？ | ❌ 否 |
| 評估 | **RLC 建議有效，但非緊迫優化** |

---

## 總結與建議

### 四個疑慮的最終評估

| # | 疑慮 | RLC 判斷 | 原始碼驗證 | 設計文件狀態 |
|---|------|----------|------------|--------------|
| 1 | GcBox Header | ✅ 有效建議 | ✅ 正確（40 bytes） | ✅ 有提及 |
| 2 | 保守掃描限制 | ✅ 有效擔憂 | ✅ 正確（但有緩解） | ⚠️ 部分漏寫 |
| 3 | 安全點可靠性 | ⚠️ 嚴重風險 | ✅ 正確（嚴重） | ❌ **嚴重漏寫** |
| 4 | 原子操作開銷 | ✅ 有效建議 | ✅ 正確 | ❌ 漏寫 |

### 建議行動項目

#### 高優先級

1. **更新設計文件**
   - 新增 `safepoint()` API 文件
   - 說明無限迴圈風險及緩解措施
   - 記錄 `clear_registers()` 和 `MASK` 機制

2. **解決 Stop-Forever 風險**
   - 評估計時器中斷方案的可行性
   - 研究與 async Rust 整合的可能性
   - 考慮提供 `sync::Gc<T>` 自動安全點機制

#### 中優先級

3. **Header 優化**
   - 評估 VTable 方案的實作複雜度
   - 進行效能基準測試比較

4. **Cache Coherence 優化**
   - 評估 TLAB 緩衝標記方案
   - 進行多執行緒標記效能分析

#### 低優先級

5. **精確堆疊掃描研究**
   - 研究 Rust 編譯器插件可行性
   - 評估 proc macro 方案的覆蓋率

---

## 附錄：相關檔案位置

| 檔案 | 路徑 |
|------|------|
| GcBox 定義 | `crates/rudo-gc/src/ptr.rs:22-37` |
| 堆疊掃描 | `crates/rudo-gc/src/stack.rs:136-230` |
| 安全點檢查 | `crates/rudo-gc/src/heap.rs:183-190` |
| safepoint API | `crates/rudo-gc/src/gc/gc.rs:183-203` |
| 導出宣告 | `crates/rudo-gc/src/lib.rs:84` |
| 設計文件 | `docs/rudo-gc-design-analysis.md` |

---

> 本文件基於 2026-01-29 原始碼驗證生成。
