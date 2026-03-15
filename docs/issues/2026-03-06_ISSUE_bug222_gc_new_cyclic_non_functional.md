# [Bug]: Gc::new_cyclic 函數存在但無法正常運作

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 用戶可能會嘗試使用此函數建立自引用結構 |
| **Severity (嚴重程度)** | High | 函數無法達成其預期目的，會導致程式行為錯誤 |
| **Reproducibility (重現難度)** | Low | 容易重現，直接使用即可观察到问题 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::new_cyclic` (ptr.rs:1039-1073)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

`Gc::new_cyclic` 函數被設計用於建立自引用（self-referential）的垃圾回收物件，但該函數目前無法正常運作。

### 預期行為 (Expected Behavior)
用戶應該能夠在閉包中建立指向 self 的 `Gc` 指標，例如：
```rust
let node = Gc::new_cyclic(|this| Node {
    value: 42,
    self_ref: Some(this), // this 應該指向新建立的物件
});
```

### 實際行為 (Actual Behavior)
1. 函數傳遞一個「dead」的 `Gc`（指標為 null）給閉包
2. 任何在閉包中儲存的自引用都會是 null 指針
3. Rehydration 機制無法定義（因 Rust 型別擦除）
4. 測試明確檢查並預期此功能可能失敗：`Gc::is_dead_or_unrooted(inner_gc)`

相關程式碼（ptr.rs:2488-2501）：
```rust
// FIXME: Self-referential cycle support is not implemented.
//
// Rehydration requires type information to ensure we only
// rehydrate dead Gc<T> references that point to the same
// allocation. Due to type erasure in our current design,
// we cannot safely verify type compatibility here.
//
// Until this is implemented, new_cyclic should be considered
// non-functional and should not be used.
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題出在 `new_cyclic` 的實作方式：

1. **傳遞 null 指針**（ptr.rs:1043-1048）：
   ```rust
   let dead_gc = Self {
       ptr: AtomicNullable::null(),
       _marker: PhantomData,
   };
   
   let value = data_fn(dead_gc); // 傳遞 null Gc 給閉包
   ```

2. **Rehydration 機制失敗**（ptr.rs:2482-2509）：
   - 由於 Rust 的型別擦除，無法驗證型別相容性
   - Rehydrator 無法安全地將 dead pointer 轉換為有效的 Gc

3. **API 設計問題**：
   - 函數簽名為 `FnOnce(Self) -> T`，其中 `Self` 是 `Gc<T>`
   - 但傳遞的是一個指標為 null 的無效 Gc

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::cell::RefCell;

#[derive(Trace)]
struct SelfRefNode {
    value: i32,
    self_ref: RefCell<Option<Gc<Self>>>,
}

#[test]
fn test_new_cyclic_with_immediate_self_ref() {
    // 這應該一個自引用結構建立，但無法正常運作
    let node = Gc::new_cyclic(|this| SelfRefNode {
        value: 100,
        self_ref: RefCell::new(Some(this)), // this 是 null Gc
    });

    // 檢查內部引用 - 由於實現限制，可能為 dead
    let inner = node.self_ref.borrow();
    if let Some(ref inner_gc) = *inner {
        // 由於 rehydration 不工作，這個 GC 可能是 dead
        if !Gc::is_dead_or_unrooted(inner_gc) {
            assert!(Gc::ptr_eq(&node, inner_gc));
        }
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 選項 1：移除函數
完全移除 `new_cyclic` 函數，因為 `new_cyclic_weak` 已經提供了正確的替代方案。

### 選項 2：修復實現
要正確實現 `new_cyclic`，需要：
1. 在 `GcBox` 中儲存唯一的 allocation ID
2. 在 rehydration 時驗證 ID 匹配
3. 這需要重大的 API 變更

### 選項 3：改進文件
- 將函數標記為 `#[deprecated]`
- 在文件中明確說明「此函數完全無法運作，請使用 `new_cyclic_weak`」
- 添加編譯時期警告

**目前狀態**：函數已經有 `#[deprecated]` 標記，但 `note` 訊息可以更明確地說明「無法運作」而非「使用 new_cyclic_weak 代替」。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在傳統 GC（如 Chez Scheme）中，自引用通常透過先分配物件再填入欄位來建立。Rust 的 immutable-by-default 哲學需要不同的方法。`new_cyclic_weak` 使用 `Weak<T>` 是正確的方向，因為它利用現有的 weak reference 機制。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題（不會導致 UB），而是 API 可用性問題。deprecated 警告應該足夠防止用戶誤用。

**Geohot (Exploit 攻擊觀點):**
攻擊者不太可能利用這個問題，因為它不會導致記憶體安全問題。最多只能導致 DoS（程式崩潰）。

---

## 驗證狀態

此問題已通過閱讀程式碼和測試驗證。`new_cyclic` 無法達成其預期目的，應該被視為 bug。

---

## Resolution (2026-03-14)

**Fix applied (Option 3):** Improved the deprecation note to explicitly state that the function is non-functional. Changed from "Self-referential cycles are not supported" to "This function is non-functional: the closure receives a null Gc. Use `new_cyclic_weak` instead." This makes it clear to users that the function cannot achieve its intended purpose and directs them to the working alternative.
