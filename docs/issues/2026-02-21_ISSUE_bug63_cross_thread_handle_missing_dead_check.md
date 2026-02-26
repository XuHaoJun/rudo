# [Bug]: Gc::cross_thread_handle() 與 Gc::weak_cross_thread_handle() 缺少 dead_flag / dropping_state 檢查

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在物件已标记为 dead 或 dropping 状态时仍持有 Gc |
| **Severity (嚴重程度)** | High | 可能导致在已drop的物件上创建handle，导致内存不安全 |
| **Reproducibility (復現難度)** | Medium | 需要特定时序触发 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::cross_thread_handle()` and `Gc::weak_cross_thread_handle()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當呼叫 `Gc::cross_thread_handle()` 或 `Gc::weak_cross_thread_handle()` 時，如果物件已經死亡（`dead_flag` 設定）或正在被 drop（`dropping_state != 0`），應該 panic 或返回錯誤。

這與以下方法的行為一致：
- `Gc::clone()` - 檢查 dead_flag 和 dropping_state
- `Gc::downgrade()` - 檢查 dead_flag 和 dropping_state

### 實際行為 (Actual Behavior)

目前 `cross_thread_handle()` 和 `weak_cross_thread_handle()` **沒有**檢查：
- `has_dead_flag()`
- `dropping_state()`

直接在 `ptr.rs:1267-1292` (`cross_thread_handle`) 和 `ptr.rs:1313-1325` (`weak_cross_thread_handle`) 中調用 `inc_ref()` / `inc_weak()` 而不檢查物件狀態。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `ptr.rs:1267-1292` 和 `ptr.rs:1313-1325`

對比 `Gc::downgrade()` (ptr.rs:1192-1206) 有正確的檢查：

```rust
pub fn downgrade(gc: &Self) -> Weak<T> {
    // ...
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::downgrade: Gc is dead or in dropping state"
        );
        (*gc_box_ptr).inc_weak();
    }
    // ...
}
```

但 `weak_cross_thread_handle()` 缺少這些檢查：

```rust
pub fn weak_cross_thread_handle(&self) -> crate::handles::WeakCrossThreadHandle<T> {
    unsafe {
        (*self.as_non_null().as_ptr()).inc_weak();  // 沒有檢查！
    }
    // ...
}
```

同樣，`cross_thread_handle()` 也缺少檢查：

```rust
pub fn cross_thread_handle(&self) -> crate::handles::GcHandle<T> {
    // ...
    unsafe { (*ptr.as_ptr()).inc_ref() };  // 沒有檢查！
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 创建一个 Gc
    let gc = Gc::new(Data { value: 42 });
    
    // 2. 强制触发 GC 来 drop 这个对象
    // (需要通过特定方式让对象被标记为 dead)
    collect_full();
    
    // 3. 此时 gc 应该被视为 "dead"，但 Gc 本身结构仍然有效
    // (ptr not null)
    
    // 4. 调用 weak_cross_thread_handle - 应该 panic 或返回错误
    // 但实际上会成功创建 handle 并增加 weak_count
    let _weak_handle = gc.weak_cross_thread_handle();
    
    // 类似地，cross_thread_handle 也会有问题
    // let _handle = gc.cross_thread_handle();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cross_thread_handle()` 中添加检查：

```rust
pub fn cross_thread_handle(&self) -> crate::handles::GcHandle<T> {
    // ... existing code ...
    
    let ptr = self.as_non_null();
    
    // 新增: 检查 dead_flag 和 dropping_state
    unsafe {
        assert!(
            !(*ptr.as_ptr()).has_dead_flag() && (*ptr.as_ptr()).dropping_state() == 0,
            "Gc::cross_thread_handle: cannot create handle for dead or dropping Gc"
        );
        (*ptr.as_ptr()).inc_ref();
    }
    
    // ... rest of code ...
}
```

在 `weak_cross_thread_handle()` 中添加检查：

```rust
pub fn weak_cross_thread_handle(&self) -> crate::handles::WeakCrossThreadHandle<T> {
    // 新增: 检查 dead_flag 和 dropping_state
    unsafe {
        let gc_box = &*self.as_non_null().as_ptr();
        assert!(
            !gc_box.has_dead_flag() && gc_box.dropping_state() == 0,
            "Gc::weak_cross_thread_handle: cannot create handle for dead or dropping Gc"
        );
        gc_box.inc_weak();
    }
    
    // ... rest of code ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
當物件被標記為 dead 或正在 dropping 時，不應該允許建立新的 handle。這與 reference counting 的基本原則不符：為一個已經無效的物件增加引用計數會導致不正確的記憶體管理。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。允許為已死亡或正在 drop 的物件建立 handle 可能導致：
1. 為無效物件增加引用計數
2. 潛在的 use-after-free
3. 與其他 GC 操作衝突

**Geohot (Exploit 攻擊觀點):**
此漏洞可以被利用來：
1. 繞過 GC 的安全檢查
2. 創建對已釋放物件的引用
3. 導致記憶體管理不一致

---

## Resolution

**已修復** - 2026-02-22

在 `ptr.rs:1267-1293` 的 `cross_thread_handle()` 中添加了 `has_dead_flag()` 和 `dropping_state()` 檢查。

在 `ptr.rs:1313-1325` 的 `weak_cross_thread_handle()` 中添加了同樣的檢查。

對比 `Gc::clone()` 和 `Gc::downgrade()` 的行為，現在這兩個方法在物件已死亡或正在 dropping 時會 panic。
