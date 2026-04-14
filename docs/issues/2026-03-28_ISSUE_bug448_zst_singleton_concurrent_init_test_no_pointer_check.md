# [Bug]: test_zst_singleton_concurrent_init 不驗證 singleton 指標相等性

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 測試本身有 bug，永遠會通過但沒有驗證任何東西 |
| **Severity (嚴重程度)** | Medium | 測試失效導致並發初始化 bug 可能漏測 |
| **Reproducibility (復現難度)** | Very High | 測試必現（但沒有驗證正確行為） |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `zst_singleton_immortal.rs::test_zst_singleton_concurrent_init`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
測試應該驗證：10 個並發執行緒都獲得相同的 ZST singleton 指標。

### 實際行為 (Actual Behavior)
測試只驗證：10 個執行緒都完成了執行（`inits.load() == 10`）。即使每個執行緒獲得不同的指標，測試也會通過。

---

## 🔬 根本原因分析 (Root Cause Analysis)

測試程式碼：
```rust
#[test]
fn test_zst_singleton_concurrent_init() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    let inits = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(10));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let barrier = barrier.clone();
            let inits = inits.clone();
            thread::spawn(move || {
                barrier.wait();
                let unit = Gc::new(());  // 每個線程創建 Gc
                drop(unit);               // 立即 drop
                inits.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(inits.load(Ordering::SeqCst), 10);  // 只驗證完成，不驗證指標相等！
}
```

**問題**：
1. 測試名稱是 `test_zst_singleton_concurrent_init`，暗示要測試「並發初始化返回相同 singleton」
2. 但 `assert_eq!(inits.load(Ordering::SeqCst), 10)` 只驗證了執行緒完成數量
3. 從未比較任何指標，因此無法檢測「每個執行緒獲得不同指標」的情況

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

修復後的測試應該是：
```rust
#[test]
fn test_zst_singleton_concurrent_init() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    let pointers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let barrier = Arc::new(std::sync::Barrier::new(10));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let barrier = barrier.clone();
            let pointers = pointers.clone();
            thread::spawn(move || {
                barrier.wait();
                let unit = Gc::new(());
                let ptr = Gc::into_raw(unit);
                pointers.lock().unwrap().push(ptr);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    let pointers = pointers.lock().unwrap();
    assert_eq!(pointers.len(), 10);
    
    // 驗證所有指標都相等
    for ptr in pointers.iter() {
        assert!(Gc::ptr_eq(&unsafe { Gc::from_raw(*ptr) }, &unsafe { Gc::from_raw(pointers[0]) }));
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `crates/rudo-gc/tests/zst_singleton_immortal.rs` 中的 `test_zst_singleton_concurrent_init`：
1. 收集每個執行緒的 `Gc<()>` 指針
2. 執行緒完成後，驗證所有指針相等（使用 `Gc::ptr_eq`）
3. 清理收集的指標

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
ZST singleton 使用 `AtomicPtr` + CAS 實現懶初始化。実装本身是正確的（double-checked locking），但測試沒有驗證關鍵屬性：所有執行緒應該獲得相同的 singleton 地址。如果 CAS 失敗導致多個實例，測試不會捕獲。

**Rustacean (Soundness 觀點):**
測試本身的設計有缺陷：它沒有測試它聲稱要測試的內容。這是一個「假陰性」測試 - 即使並發初始化有 bug，測試也會通過。

**Geohot (Exploit 觀點):**
如果 ZST singleton 並發初始化有 race condition，可能導致：
1. 記憶體洩漏（多個實例）
2. use-after-free（如果某些執行緒使用已釋放的實例）

但更可能的是實現是正確的，只是測試不足以驗證。

---

## 修復紀錄 (Fix Record)

**Date:** 2026-03-28
**Fix:** 修改 `test_zst_singleton_concurrent_init` 以收集所有執行緒的 `Gc<()>` 指標，並使用 `Gc::ptr_eq` 驗證它們都指向相同的 singleton。修復後測試通過，確認 ZST singleton 並發初始化行為正確。