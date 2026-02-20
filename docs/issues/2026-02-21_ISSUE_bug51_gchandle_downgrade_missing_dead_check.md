# [Bug]: GcHandle::downgrade() Missing Dead/Dropping State Check

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 與 bug50 相同模式，開發者可能會依賴此行為 |
| **Severity (嚴重程度)** | Medium | 導致文件與實作不一致，可能造成預期外的行為 |
| **Reproducibility (復現難度)** | Very High | 直接檢視程式碼即可發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle<T>::downgrade()` method
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcHandle::downgrade()` 應該在物件為 dead 或正在被 drop 時進行檢查（類似 `Gc::downgrade()` 的文件描述），以確保不會創建指向無效物件的 weak reference。

### 實際行為 (Actual Behavior)
`GcHandle::downgrade()` 直接遞增 weak count，沒有檢查 `has_dead_flag()` 或 `dropping_state()`：

```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    unsafe {
        (*self.ptr.as_ptr()).inc_weak();  // 沒有任何檢查！
    }
    WeakCrossThreadHandle {
        weak: GcBoxWeakRef::new(self.ptr),
        origin_tcb: Arc::clone(&self.origin_tcb),
        origin_thread: self.origin_thread,
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/handles/cross_thread.rs:212-221`

此 bug 與 bug50 (`Gc::downgrade()`) 模式相同，但影響不同的類型：
- **bug50**: `Gc<T>::downgrade()` in `ptr.rs`
- **本 bug**: `GcHandle<T>::downgrade()` in `handles/cross_thread.rs`

雖然 `WeakCrossThreadHandle::upgrade()` 有安全檢查會對 dead 物件返回 `None`，但 `downgrade()` 本身缺少驗證會造成：
1. **語意不一致**: 與其他類似函數的行為不一致
2. **API 誤導**: 使用者可能期望 `downgrade()` 有如 `Weak::upgrade()` 的檢查
3. **文件匹配問題**: 如果文件說明應該有檢查，實作應該符合

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let handle = gc.cross_thread_handle();
    
    drop(gc);
    collect_full();
    
    // 這裡應該有檢查或 panic，但實際直接遞增 weak count
    // 會創建一個指向已回收記憶體的 WeakCrossThreadHandle
    let _ = handle.downgrade();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在遞增 weak count 之前添加驗證：

```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.has_dead_flag() && gc_box.dropping_state() == 0,
            "GcHandle::downgrade: object is dead or being dropped"
        );
        gc_box.inc_weak();
    }
    WeakCrossThreadHandle {
        weak: GcBoxWeakRef::new(self.ptr),
        origin_tcb: Arc::clone(&self.origin_tcb),
        origin_thread: self.origin_thread,
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 cross-thread 情境下，允許對 dead 物件創建 weak reference 可能導致 cross-thread weak count 不正確，進而影響跨執行緒的記憶體回收判斷。這與 bug50 的影響類似，但發生在不同的執行緒上下文。

**Rustacean (Soundness 觀點):**
這是文件與實作不一致的問題（與 bug50 相同模式）。雖然 `WeakCrossThreadHandle::upgrade()` 會檢查並返回 `None`，但 `downgrade()` 缺少驗證會造成 API 語意不一致。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個差異，在物件死亡後仍然創建 cross-thread weak reference，進一步探索記憶體佈局或進行 cross-thread 攻擊。
