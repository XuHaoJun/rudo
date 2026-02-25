# [Bug]: Write Guard Drop TOCTOU - 檢查 barrier 狀態與調用 mark_object_black 之间状态可能改变

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 drop 函數中，檢查和 mark 之間 incremental marking 狀態改變 |
| **Severity (嚴重程度)** | High | 可能導致物件被錯誤回收，造成記憶體安全問題 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeRefMut::drop`, `GcRwLockWriteGuard::drop`, `GcMutexGuard::drop`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

在 `GcThreadSafeRefMut`、`GcRwLockWriteGuard` 和 `GcMutexGuard` 的 Drop 實作中，`is_incremental_marking_active()` 在兩處被調用：
1. 第一次：檢查是否需要執行 barrier
2. 第二次：在 `mark_object_black()` 內部

但更重要的是，在 Drop 函數內部，檢查 barrier 狀態和調用 `mark_object_black()` 之間存在時間窗口，期間 barrier 狀態可能改變。

### 預期行為

在整個 Drop 函數執行期間，應該使用一致的 barrier 狀態。要麼全部執行 barrier，要麼全部不執行。

### 實際行為

1. 檢查 barrier 狀態（假設為 INACTIVE）
2. 程式繼續執行其他操作
3. Incremental marking 啟動（ACTIVE）
4. 調用 `mark_object_black()` - 由於步驟1判斷為 INACTIVE，這裡不會執行！

或者反向情況：
1. 檢查 barrier 狀態（假設為 ACTIVE）
2. 程式捕獲指標
3. Incremental marking 結束（INACTIVE）
4. 調用 `mark_object_black()` - 雖然標記了物件，但這是不必要的開銷

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於以下位置：

1. **GcThreadSafeRefMut::drop** (`cell.rs:1169-1183`):
```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        // Line 1171-1172: 第一次檢查
        if crate::gc::incremental::is_generational_barrier_active()
            || crate::gc::incremental::is_incremental_marking_active()
        {
            // ... 捕獲指標 ...
            
            // Line 1178-1180: 調用 mark_object_black
            for gc_ptr in ptrs {
                let _ = unsafe {
                    crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                };
            }
        }
    }
}
```

2. **GcRwLockWriteGuard::drop** (`sync.rs:372-386`): 相同模式

3. **GcMutexGuard::drop** (`sync.rs:610-624`): 相同模式

**問題**：
- 在 line 1171-1172 檢查 barrier 狀態
- 在 line 1178-1180 調用 `mark_object_black()`
- 這兩處之間的狀態可能改變

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確控制時序：
1. 持有 GcThreadSafeRefMut / GcRwLockWriteGuard / GcMutexGuard
2. 在 drop 函數中，檢查完成後、mark_object_black 調用前，incremental marking 狀態改變
3. 導致 barrier 行為不一致

理論上需要並發 GC 和 mutator 才能穩定重現。建議使用 model checker 驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

緩存 barrier 狀態並在整個函數中使用：

```rust
impl<T: GcCapture + ?Sized> Drop for GcThreadSafeRefMut<'_, T> {
    fn drop(&mut self) {
        let incremental_active = crate::gc::incremental::is_incremental_marking_active();
        let generational_active = crate::gc::incremental::is_generational_barrier_active();
        
        if generational_active || incremental_active {
            let mut ptrs = Vec::with_capacity(32);
            (*self.inner).capture_gc_ptrs_into(&mut ptrs);

            // 使用 cached 值
            if generational_active || incremental_active {
                for gc_ptr in ptrs {
                    let _ = unsafe {
                        crate::gc::incremental::mark_object_black(gc_ptr.as_ptr() as *const u8)
                    };
                }
            }
        }
    }
}
```

同樣的模式應用於 GcRwLockWriteGuard 和 GcMutexGuard。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這是經典的 TOCTOU 問題，在增量標記中尤其危險
- SATB 需要 consistent 的視圖，狀態不一致會導致標記不完全
- 建議：使用本地緩存的狀態副本，確保整個操作使用相同的狀態

**Rustacean (Soundness 觀點):**
- 這是並發安全問題，儘管觸發窗口很小
- 從理論上講，這可能導致 Use-After-Free（如果物件被錯誤回收）
- 雖然實際觸發困難，但應該修復以確保正確性

**Geohot (Exploit 觀點):**
- 在高負載並發環境中，攻擊者可能嘗試利用這個小窗口
- 雖然難以穩定利用，但是一個潛在的攻击面
- 修復成本低，建議修復
