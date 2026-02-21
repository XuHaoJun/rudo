# [Bug]: GcRwLock 與 GcMutex 的 write()/lock() 缺少 SATB 舊值捕獲，導致增量標記期間潛在 UAF

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 incremental marking 啟用時，同時有 OLD→YOUNG 引用被覆寫 |
| **Severity (嚴重程度)** | Critical | 可能導致 UAF，記憶體安全威脅 |
| **Reproducibility (復現難度)** | High | 需精確控制 incremental marking 時序與 OLD→YOUNG 引用 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** GcRwLock, GcMutex
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`GcRwLock::write()`、`GcRwLock::try_write()`、`GcMutex::lock()` 與 `GcMutex::try_lock()` 在獲得可變引用後，只觸發了世代寫屏障（generational write barrier），但**缺少 SATB 舊值捕獲**。

這與 `GcCell::borrow_mut()` 的行為不一致。`GcCell::borrow_mut()` 會：
1. 在取得可變引用**之前**：捕獲舊的 GC 指針值並呼叫 `record_satb_old_value()`
2. 在 Drop **之後**：標記新的 GC 指針值為黑色（這部分在 GcRwLockWriteGuard/GcMutexGuard 的 Drop 中已修復 - 參考 bug18/bug59）

### 預期行為
在 incremental marking 期間，`GcRwLock::write()` 與 `GcMutex::lock()` 應該在覆寫舊的 GC 指針之前，呼叫 `record_satb_old_value()` 捕獲舊值。

### 實際行為
`GcRwLock::write()` 僅呼叫 `trigger_write_barrier()`（設定 dirty bit 並加入 dirty list），但**沒有**呼叫 `record_satb_old_value()` 捕獲舊值。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `sync.rs` 中，`GcRwLock::write()` 實作：
```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    self.trigger_write_barrier();  // 只觸發世代屏障
    let guard = self.inner.write();
    GcRwLockWriteGuard { guard, _marker: PhantomData }
}
```

而 `cell.rs` 的 `GcCell::borrow_mut()` 正確實作：
```rust
if crate::gc::incremental::is_incremental_marking_active() {
    let value = &*self.inner.as_ptr();  // 取得舊值
    let mut gc_ptrs = Vec::with_capacity(32);
    value.capture_gc_ptrs_into(&mut gc_ptrs);  // 捕獲舊 GC 指針
    if !gc_ptrs.is_empty() {
        crate::heap::with_heap(|heap| {
            for gc_ptr in gc_ptrs {
                if !heap.record_satb_old_value(gc_ptr) {  // 記錄舊值！
                    // fallback...
                }
            }
        });
    }
}
```

缺少 `record_satb_old_value()` 調用會導致：
1. OLD→YOUNG 引用被覆寫時，舊值未被記錄
2. 如果舊值是物件的唯一引用，該物件可能被錯誤回收
3. 後續存取已回收物件導致 UAF

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要精確時序控制，PoC 難度較高。概念上：
1. 建立 OLD 物件（透過 `collect_full()` 提升）
2. 建立 OLD→YOUNG 引用存於 `GcRwLock<T>` 內部
3. 啟動 incremental marking
4. 呼叫 `gc_rwlock.write()` 覆寫 OLD→YOUNG 為新值
5. 如果舊 YOUNG 物件無其他引用，會被錯誤回收

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `sync.rs` 的 `GcRwLock::write()`、`GcRwLock::try_write()`、`GcMutex::lock()`、`GcMutex::try_lock()` 中，新增 SATB 舊值捕獲：

```rust
pub fn write(&self) -> GcRwLockWriteGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.write();
    
    // 捕獲舊值 for SATB
    if crate::gc::incremental::is_incremental_marking_active() {
        unsafe {
            let value = &*guard;
            let mut gc_ptrs = Vec::with_capacity(32);
            value.capture_gc_ptrs_into(&mut gc_ptrs);
            if !gc_ptrs.is_empty() {
                crate::heap::with_heap(|heap| {
                    for gc_ptr in gc_ptrs {
                        if !heap.record_satb_old_value(gc_ptr) {
                            crate::gc::incremental::IncrementalMarkState::global()
                                .request_fallback(
                                    crate::gc::incremental::FallbackReason::SatbBufferOverflow,
                                );
                            break;
                        }
                    }
                });
            }
        }
    }
    
    self.trigger_write_barrier();  // 保持世代屏障
    GcRwLockWriteGuard { guard, _marker: PhantomData }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB (Snapshot-At-The-Beginning) 的核心是「在 mutation 發生前，記錄所有可達的 GC 指標」。`GcCell::borrow_mut()` 正確實作了這點，但 `GcRwLock::write()` 與 `GcMutex::lock()` 漏掉了舊值捕獲。這會破壞 incremental marking 的正確性，導致部分存活物件被錯誤回收。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。如果 UAF 發生，可能導致各種未定義行為，包括讀取已釋放記憶體、雙重釋放等。此問題僅在 `incremental` feature 啟用時顯現。

**Geohot (Exploit 觀點):**
攻擊者可能透過控制 GC 時序與資料流，刻意觸發此 UAF 漏洞。但此 bug 需要多個條件同時滿足（incremental marking + OLD→YOUNG 引用 + 唯一引用），利用難度中等。
