# [Bug]: GcHandle::resolve() 在原始執行緒終止後 panic

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 GcHandle 比原始執行緒壽命更長時觸發 |
| **Severity (嚴重程度)** | Medium | 造成 confusing panic，不是記憶體安全問題 |
| **Reproducibility (復現難度)** | Low | 容易重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve`, `CrossThreadHandle`, `ThreadControlBlock`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當原始執行緒（origin thread）終止後，持有 `GcHandle` 的程式碼嘗試調用 `resolve()` 時會 panic。雖然 TCB 透過 Arc 保持活力（這對記憶體安全是正確的），但執行緒 ID 檢查永遠不會成功，因為原始執行緒已經不存在了。

### 預期行為
- `resolve()` 應該返回有意義的錯誤（例如 `None`）而不是 panic
- 或者文檔應該清楚說明這個限制

### 實際行為
1. 執行緒 A 創建 `GcHandle`
2. 執行緒 A 終止
3. 其他執行緒持有 `GcHandle` 並嘗試調用 `resolve()`
4. **Panic**：`GcHandle::resolve() must be called on the origin thread`

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs:133-146` 的 `resolve()` 方法中：

```rust
#[track_caller]
pub fn resolve(&self) -> Gc<T> {
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        "GcHandle::resolve() must be called on the origin thread \
         (origin={:?}, current={:?})",
        self.origin_thread,
        std::thread::current().id(),
    );
    // SAFETY: The root registration guarantees the object is alive.
    // We've verified we're on the origin thread, so producing a Gc<T>
    // is safe even if T: !Send.
    unsafe { Gc::from_raw(self.ptr.as_ptr() as *const u8) }
}
```

問題：
- 當原始執行緒終止後，`std::thread::current().id()` 永遠不會等於 `self.origin_thread`
- 這是一個 runtime assertion，會導致 panic
- 沒有辦法從終止的執行緒中「resolve」物件，因為執行緒已經不存在
- `try_resolve()` 方法也有同樣的問題 (`handles/cross_thread.rs:169-174`)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let handle = std::thread::spawn(|| {
        let gc = Gc::new(Data { value: 42 });
        gc.cross_thread_handle()
    })
    .join()
    .unwrap();

    // 這裡會 panic，因為原始執行緒已經終止
    let result = std::panic::catch_unwind(|| {
        handle.resolve()
    });
    
    assert!(result.is_err(), "預期 panic，實際上發生了什麼？");
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：改進錯誤訊息

在文檔中清楚說明這個限制：

```rust
/// # Panics
///
/// 如果原始執行緒已終止，此方法會 panic。
/// 這是因為無法從不存在的執行緒中解析物件。
///
/// 如果您需要處理執行緒終止的情況，請使用其他機制
/// （例如將物件移動到共享的 GC heap）。
pub fn resolve(&self) -> Gc<T> {
    // ...
}
```

### 方案 2：返回 Result

```rust
pub fn resolve(&self) -> Result<Gc<T>, ResolveError> {
    if std::thread::current().id() != self.origin_thread {
        return Err(ResolveError::OriginThreadTerminated);
    }
    // ...
}

#[derive(Debug)]
pub enum ResolveError {
    OriginThreadTerminated,
}
```

### 方案 3：自動遷移到調用執行緒

這是一個更複雜的方案，需要改變設計。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是執行緒生命週期管理的問題。在傳統 GC 中，所有物件都在共享的 heap 中，沒有執行緒本地的概念。rudo-gc 的設計要求 `resolve()` 在原始執行緒調用，這對於 `!Send` 類型是安全的，但需要更好的錯誤處理。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，而是 API 可用性問題。Panic 是有道理的（你不能從不存在的執行緒中解析資料），但應該有更好的錯誤處理。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過：
1. 等待目標執行緒終止
2. 嘗試調用 resolve() 觸發 panic
3. 觀察 panic 訊息可能洩露執行緒 ID 資訊
