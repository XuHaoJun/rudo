# [Bug]: ZST Singleton 初始化時 ref_count 為 2 而非 1

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 每次建立 Gc<ZST> 時都會觸發 |
| **Severity (嚴重程度)** | Low | 不會造成明顯問題（ZST 是 immortal） |
| **Reproducibility (復現難度)** | Very High | 需要直接檢查 internal ref_count |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::new_zst()`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
- 當建立新的 `Gc<ZST>` 時，internal ref_count 應該從 0 開始
- 建立後 ref_count 應該為 1（與非 ZST Gc 一致）

### 實際行為 (Actual Behavior)
在 `ptr.rs:new_zst()` 函數中：

```rust
// 初始化時 ref_count = 1
gc_box.write(GcBox {
    ref_count: AtomicUsize::new(1),  // 初始值為 1
    weak_count: AtomicUsize::new(1),  // 標記為 immortal
    ...
});

// 之後又遞增 ref_count
unsafe {
    (*gc_box_ptr).inc_ref();  // ref_count 變成 2！
}
```

這導致第一個 `Gc<ZST>` 的 ref_count 為 2，而非預期的 1。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在 `ptr.rs:795-835`：

1. 當創建新的 ZST 分配時：
   - `ref_count` 初始化為 1
   - `weak_count` 初始化為 1（作為 immortal 標記）

2. 之後執行 `inc_ref()`：
   - `ref_count` 從 1 遞增到 2

3. 這與非 ZST Gc 的行為不一致：
   - 非 ZST：`ref_count` 初始化為 1，不執行額外的 `inc_ref()`
   - ZST：`ref_count` 初始化為 1，然後 `inc_ref()` 變成 2

為什麼這不是一個嚴重的問題：
- ZST 使用 singleton 模式，所有 Gc<ZST> 指向同一個分配
- `weak_count = 1` 使 ZST 成為 immortal，不會被 GC 回收
- ref_count 永遠不會降到 0，所以物件永遠不會被釋放

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Empty;

fn main() {
    let zst = Gc::new(());
    let initial_rc = Gc::ref_count(&zst).get();
    
    println!("Initial ref_count: {}", initial_rc);
    // 輸出：Initial ref_count: 2
    // 預期：Initial ref_count: 1
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

方案 1：初始化 ref_count 為 0
```rust
gc_box.write(GcBox {
    ref_count: AtomicUsize::new(0),  // 改為 0
    weak_count: AtomicUsize::new(1),
    ...
});

// 遞增到 1
unsafe {
    (*gc_box_ptr).inc_ref();
}
```

方案 2：初始化後不執行 inc_ref（如果這是預期行為）
```rust
gc_box.write(GcBox {
    ref_count: AtomicUsize::new(1),  // 保持為 1
    weak_count: AtomicUsize::new(1),
    ...
});

// 刪除 inc_ref() 調用
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
ZST 使用 singleton 模式是合理的設計，但 internal ref_count 的不一致性可能會造成將來的混淆。建議修復以保持與非 ZST Gc 的一致性。

**Rustacean (Soundness 觀點):**
這不是一個 soundness 問題，因為 ZST 是 immortal（由 weak_count=1 保護）。但這是一個內部一致性问题。

**Geohot (Exploit 攻擊觀點):**
在此情況下沒有明顯的攻擊面，因為 ZST 是 immortal。

---

## 備註

- 此 bug 不會造成實際的記憶體問題，因為 ZST 是 immortal
- 現有測試 `test_zst_singleton_ref_count_maintained` 只檢查相對變化，不會發現此問題
- 建議修復以保持內部一致性
