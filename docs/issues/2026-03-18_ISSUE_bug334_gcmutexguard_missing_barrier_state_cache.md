# [Bug]: GcMutexGuard 缺少 barrier state 快取，與 GcRwLockWriteGuard 不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 lock 獲取後、drop 前變更 GC 配置 |
| **Severity (嚴重程度)** | Medium | 可能導致 barrier 行為不一致，潛在記憶體安全問題 |
| **Reproducibility (復現難度)** | High | 需要精確控制 GC 配置變更的時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sync.rs` - `GcMutexGuard`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`GcMutexGuard` 應該與 `GcRwLockWriteGuard` 有一致的 barrier 行為：快取 `incremental_active` 和 `generational_active` 狀態，確保在 lock 獲取時和 drop 時使用相同的 barrier 配置。

### 實際行為
`GcMutexGuard` 在 drop 時動態重新獲取 barrier 狀態，而不是使用 lock 獲取時快取的值。這與 `GcRwLockWriteGuard` 的行為不一致，後者正確地快取了這些值。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題程式碼

**`sync.rs:433-438` - GcRwLockWriteGuard 正確快取 barrier 狀態：**
```rust
pub struct GcRwLockWriteGuard<'a, T: GcCapture + ?Sized> {
    guard: parking_lot::RwLockWriteGuard<'a, T>,
    _marker: PhantomData<&'a T>,
    incremental_active: bool,    // <-- 有這個欄位
    generational_active: bool,    // <-- 有這個欄位
}
```

**`sync.rs:590-603` - GcMutex::lock() 計算了 barrier 狀態但沒有儲存：**
```rust
pub fn lock(&self) -> GcMutexGuard<'_, T>
where
    T: GcCapture,
{
    let guard = self.inner.lock();
    let incremental_active = is_incremental_marking_active();   // 計算了
    let generational_active = is_generational_barrier_active(); // 計算了
    // ... 使用這些值 ...
    GcMutexGuard {
        guard,
        _marker: PhantomData,
        // <-- 沒有儲存 incremental_active 和 generational_active！
    }
}
```

**`sync.rs:706-709` - GcMutexGuard 結構體缺少 barrier 欄位：**
```rust
pub struct GcMutexGuard<'a, T: GcCapture + ?Sized> {
    guard: parking_lot::MutexGuard<'a, T>,
    _marker: PhantomData<&'a T>,
    // <-- 缺少 incremental_active 和 generational_active 欄位
}
```

**`sync.rs:735-758` - GcMutexGuard::drop 重新獲取狀態：**
```rust
impl<T: GcCapture + ?Sized> Drop for GcMutexGuard<'_, T> {
    fn drop(&mut self) {
        // 重新獲取！這與 GcRwLockWriteGuard 不一致
        let incremental_active = is_incremental_marking_active();
        let generational_active = is_generational_barrier_active();
        // ...
    }
}
```

### 邏輯缺陷
1. 執行緒 A 獲取 `GcMutex` 的 lock
2. 計算並使用當時的 `incremental_active` 和 `generational_active` 狀態
3. 在 guard drop 之前，GC 配置變更（例如：啟用/停用 incremental marking）
4. Drop 時重新獲取狀態，使用與 lock 獲取時不同的配置
5. 結果：barrier 行為可能不一致

### 與 GcRwLockWriteGuard 的比較

`GcRwLockWriteGuard` 正確地快取了這些值：
```rust
// lock() 時
GcRwLockWriteGuard {
    guard,
    _marker: PhantomData,
    incremental_active,  // <-- 儲存
    generational_active, // <-- 儲存
}

// drop() 時使用儲存的值
fn drop(&mut self) {
    let incremental_active = self.incremental_active; // <-- 使用儲存的值
    let generational_active = self.generational_active;
    // ...
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcMutexGuard` 結構體中添加與 `GcRwLockWriteGuard` 相同的欄位：

```rust
pub struct GcMutexGuard<'a, T: GcCapture + ?Sized> {
    guard: parking_lot::MutexGuard<'a, T>,
    _marker: PhantomData<&'a T>,
    incremental_active: bool,    // 新增
    generational_active: bool,   // 新增
}
```

然後在 `lock()` 和 `try_lock()` 方法中填入這些欄位：

```rust
GcMutexGuard {
    guard,
    _marker: PhantomData,
    incremental_active, // 新增
    generational_active, // 新增
}
```

最後修改 `Drop` implementation 使用儲存的值：

```rust
impl<T: GcCapture + ?Sized> Drop for GcMutexGuard<'_, T> {
    fn drop(&mut self) {
        let incremental_active = self.incremental_active; // 使用儲存的值
        let generational_active = self.generational_active;
        // ...
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GC 的 barrier 行為必須在物件生命週期內保持一致。如果在 lock 獲取時記錄了 SATB，drop 時應該使用相同的配置。動態變更 barrier 行為可能導致標記不正確或遺漏。

**Rustacean (Soundness 觀點):**
這不是傳統的 UB，但可能導致記憶體安全問題。如果 incremental marking 配置在關鍵時間點變更，可能導致物件被錯誤地回收。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能嘗試在 lock 獲取和 drop 之間觸發 GC 配置變更，導致不一致的 barrier 行為。在極端情況下，這可能導致 use-after-free。

---

## ✅ Fix Applied (2026-03-19)

**Fix:** Added `incremental_active` and `generational_active` fields to `GcMutexGuard` struct to cache barrier state, matching `GcRwLockWriteGuard`.

**Changes in `sync.rs`:**
1. Added `incremental_active: bool` and `generational_active: bool` fields to `GcMutexGuard` struct (line 709-710)
2. Updated `lock()` method to populate cached values (lines 604-607)
3. Updated `try_lock()` method to populate cached values (lines 645-648)
4. Updated `Drop` implementation to use cached values instead of dynamically fetching (lines 743-744)

**Verification:**
- Build: ✅ `cargo build --workspace` succeeds
- Clippy: ✅ No warnings
- Tests: ✅ All tests pass
