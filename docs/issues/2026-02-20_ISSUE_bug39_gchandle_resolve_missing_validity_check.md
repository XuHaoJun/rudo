# [Bug]: GcHandle::resolve() 缺少物件有效性驗證

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要非常特定的前提條件才能觸發 |
| **Severity (嚴重程度)** | High | 可能導致 use-after-free |
| **Reproducibility (復現難度)** | Low | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`, `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcHandle::resolve()` 和 `GcHandle::try_resolve()` 方法直接存取內部指標，沒有驗證物件是否仍然有效。雖然 GcHandle 持有 root entry 應該防止物件被回收，但缺少明確的驗證可能在某些邊界情況下導致問題。

### 預期行為
- `resolve()` 應該在返回前驗證物件仍然有效
- 或者在文檔中明確說明為什麼不需要驗證

### 實際行為
`resolve()` 直接遞增 ref_count 並返回 Gc，沒有任何有效性檢查：
```rust
pub fn resolve(&self) -> Gc<T> {
    assert_eq!(...); // 只檢查執行緒
    unsafe {
        (*self.ptr.as_ptr()).inc_ref(); // 直接存取指標，沒有驗證
        Gc::from_raw(self.ptr.as_ptr() as *const u8)
    }
}
```

對比 `Weak::upgrade()` 的實現：
```rust
pub fn upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire).as_option()?;
    unsafe {
        let gc_box = &*ptr.as_ptr();
        // 多重檢查
        if gc_box.is_under_construction() { return None; }
        if gc_box.has_dead_flag() { return None; }
        // ...
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs:142-157`：

```rust
pub fn resolve(&self) -> Gc<T> {
    assert_eq!(
        std::thread::current().id(),
        self.origin_thread,
        ...
    );
    // 缺少物件有效性驗證！
    unsafe {
        (*self.ptr.as_ptr()).inc_ref();
        Gc::from_raw(self.ptr.as_ptr() as *const u8)
    }
}
```

問題：
1. 沒有檢查指標是否為 null
2. 沒有檢查物件是否正在構造中
3. 沒有檢查物件是否已被標記為 dead
4. 沒有檢查 ref_count 是否為 0

雖然 GcHandle 持有 root entry 應該防止物件被回收，但在以下邊界情況下可能出問題：
- Root entry 被意外移除
- 記憶體損壞
- 其他導致指標變無效的 bug

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

這個 bug 需要非常特定的條件才能觸發，可能難以穩定重現。理論上的攻擊場景：

```rust
// 理論上的 PoC - 需要精確控制
use rudo_gc::{Gc, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    // 理論上：如果 root entry 被意外移除
    // resolve() 可能會訪問已釋放的記憶體
    
    let resolved = handle.resolve();
    println!("{}", resolved.value);
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：添加有效性檢查（推薦）
在 `resolve()` 中添加與 `Weak::upgrade()` 類似的檢查：

```rust
pub fn resolve(&self) -> Gc<T> {
    assert_eq!(...);
    
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    
    unsafe {
        let gc_box = &*gc_box_ptr;
        
        // 添加檢查
        assert!(!gc_box.is_under_construction(), "...");
        assert!(!gc_box.has_dead_flag(), "...");
        
        (*gc_box_ptr).inc_ref();
        Gc::from_raw(gc_box_ptr as *const u8)
    }
}
```

### 方案 2：文檔說明（如果這是設計決策）
在文檔中明確說明為什麼不需要檢查：
- GcHandle 持有 root entry 防止物件被回收
- Root entry 的存在保證指標有效性

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在傳統 GC 實現中，handle 或 root 應該始終保持對物件的引用。如果物件被回收，相應的 handle 應該被標記為無效或移除。明确的驗證可以防止潛在的記憶體安全問題。

**Rustacean (Soundness 觀點):**
這是一個潛在的記憶體安全問題。雖然在正常操作下（root entry 存在）不會出問題，但缺少明確的驗證在面對記憶體損壞或其他 bug 時可能導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能夠操控記憶體或觸發特定的 race condition，他們可能：
1. 使 root entry 失效
2. 觸發 resolve()
3. 訪問已釋放的記憶體

---

## 備註

這個問題與 bug #11（GcHandle::resolve() panic when origin terminated）不同：
- bug11: 執行緒終止後調用 resolve() 會 panic
- 本 bug: 物件無效時 resolve() 可能返回 use-after-free
