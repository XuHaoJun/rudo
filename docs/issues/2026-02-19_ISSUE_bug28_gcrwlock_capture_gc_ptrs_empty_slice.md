# [Bug]: GcRwLock::capture_gc_ptrs() 返回空切片導致 GC 遺漏內部指標

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 GC 在遍歷過程中使用 `capture_gc_ptrs()` 而非 `capture_gc_ptrs_into()` 時觸發 |
| **Severity (嚴重程度)** | Critical | 會導致 GC 遺漏 GcRwLock 內部的 GC 指標，造成 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要 GC 使用正確的方法路徑 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock<T>` 的 `GcCapture` 實作
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`GcRwLock<T>` 實作了 `GcCapture` trait，但其 `capture_gc_ptrs()` 方法錯誤地返回空切片 `&[]`，即使內部包含 GC 指標。這導致當 GC 使用 `capture_gc_ptrs()` 方法遍歷指標時，會遺漏 `GcRwLock` 內部的所有 GC 指標。

### 預期行為
- `capture_gc_ptrs()` 應該返回包含內部所有 GC 指標的切片
- GC 應該能夠找到並追踪 GcRwLock 內部的所有 GC 指標

### 實際行為
1. `capture_gc_ptrs()` 返回 `&[]` (空切片)
2. `capture_gc_ptrs_into()` 正確地遍歷內部指標
3. 如果 GC 使用 `capture_gc_ptrs()` 路徑，會錯過 GcRwLock 內部的指標
4. 導致內部指標被錯誤地回收 (use-after-free)

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `sync.rs:593-605`：

```rust
impl<T: GcCapture + ?Sized> GcCapture for GcRwLock<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]  // <-- BUG: 應該返回實際的 GC 指標切片
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(value) = self.inner.try_read() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

問題在於 `capture_gc_ptrs()` 返回 `&[]`，但 `capture_gc_ptrs_into()` 實際上可以遍歷並獲取內部指標。這兩個方法應該保持一致。

此外，`GcMutex<T>` 根本沒有實作 `GcCapture` trait，這也是一個遺漏。

在 `cell.rs:505` 中，預設實作使用 `capture_gc_ptrs()`：
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    let slice = self.capture_gc_ptrs();
    ptrs.extend_from_slice(slice);
}
```

如果某處直接呼叫 `capture_gc_ptrs()` 而非 `capture_gc_ptrs_into()`，會得到空切片。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcRwLock, Trace, collect_full};
use std::cell::RefCell;

#[derive(Trace)]
struct Inner {
    value: i32,
}

#[derive(Trace)]
struct Container {
    inner: GcRwLock<Inner>,
}

fn main() {
    let gc = Gc::new(Container {
        inner: GcRwLock::new(Inner { value: 42 }),
    });

    // 使用 capture_gc_ptrs 檢查
    let ptrs = gc.inner.capture_gc_ptrs();
    println!("GcRwLock capture_gc_ptrs returned {} ptrs", ptrs.len());
    // 預期: 1 (應該包含 Inner 的 GcBox)
    // 實際: 0 (BUG!)

    // 使用 capture_gc_ptrs_into 檢查
    let mut ptrs_vec = Vec::new();
    gc.inner.capture_gc_ptrs_into(&mut ptrs_vec);
    println!("GcRwLock capture_gc_ptrs_into returned {} ptrs", ptrs_vec.len());
    // 預期: 1
    // 實際: 1 (正確)
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1: 修正 GcRwLock 的 capture_gc_ptrs 實作

`capture_gc_ptrs()` 需要返回實際的 GC 指標，但不能返回 slice 參考（因為需要動態獲取）。正確的做法是：

1. 移除 `capture_gc_ptrs()` 的自訂實作，使用 default
2. 或修改為使用 thread-local buffer（複雜）

### 方案 2: 移除 GcRwLock 的 GcCapture 實作

由於 `GcRwLock` 內部的資料可能在運行時變化，無法以切片形式返回。最安全的做法是：

1. 移除 `GcCapture` for `GcRwLock` 的實作
2. 依賴 `Trace` trait 進行 GC 遍歷（已在 `unsafe impl<T: Trace + ?Sized> Trace for GcRwLock<T>` 中實作）

### 方案 3: 同時修正 GcMutex

1. 為 `GcMutex` 添加 `GcCapture` 實作（或移除，如方案 2）

建議採用方案 2，因為：
- `GcRwLock` 和 `GcMutex` 已經透過 `Trace` trait 正確實作了 GC 遍歷
- `GcCapture` 主要用於靜態可知指標的優化
- 運行時可變的內部資料不適合透過 `capture_gc_ptrs()` 返回

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 Scheme GC 中，包裝在 mutex/rwlock 中的指標遍歷是複雜的。靜態切片不適用於運行時可變的內部資料。應該依賴 `Trace` trait 進行遍歷，而不是 `GcCapture` 的切片返回。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。如果 GC 遺漏了內部的指標，會導致這些指標被錯誤地回收，後續解引用會造成 use-after-free。必須修復以確保 GC 的正確性。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以利用這個漏洞：1. 建立一個包含 GcRwLock 的物件
2. 利用 GC 遍歷路徑的差異
3. 當 GC 遺漏內部指標時，物件被錯誤回收
4. 攻擊者可以控制被回收物件的內容，實現記憶體利用

---

## Resolution

**2026-02-21** — 方案 2 + 3 (文件化 + 補齊 GcMutex):

- **GcRwLock/GcMutex** `capture_gc_ptrs()` 設計上回傳 `&[]`：lock 保護的資料無法提供靜態切片，需透過 `capture_gc_ptrs_into()` 取得指標。
- 在 `capture_gc_ptrs()` 上加註說明，要求必須使用 `capture_gc_ptrs_into()`。
- 新增 **GcMutex** 的 `GcCapture` 實作（含 `capture_gc_ptrs_into`，使用 `try_lock()`，與 GcRwLock/GcThreadSafeCell 相同）。
- SATB 與 GC 流程僅使用 `capture_gc_ptrs_into()`，無任何路徑使用 `capture_gc_ptrs()` 做指標收集。
- 新增 `test_gcrwlock_gcmutex_capture_gc_ptrs_into`，驗證兩者 `capture_gc_ptrs_into` 正確收集內部 Gc。
