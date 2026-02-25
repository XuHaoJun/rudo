# [Bug]: GcHandle/GcBoxWeakRef inc_ref TOCTOU Race - 檢查與遞增非原子操作導致 Use-After-Free

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒並發操作才能觸發 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，讀取已釋放記憶體 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`, `GcHandle::clone()`, `GcBoxWeakRef::upgrade()`, `GcBoxWeakRef::try_upgrade()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

在調用 `inc_ref()` 遞增引用計數前，應該以原子方式驗證物件狀態（未死亡、非正在 drop、非 construction 中），確保不會對已棄置的物件進行操作。

### 實際行為 (Actual Behavior)

儘管程式碼已添加 `has_dead_flag()`、`dropping_state()`、`is_under_construction()` 等檢查，但這些檢查與 `inc_ref()` 調用**不是原子操作**。存在 TOCTOU (Time-of-Check-Time-of-Use) 競爭視窗：

1. Thread A: 調用 `resolve()`，檢查 `has_dead_flag()` 返回 `false`
2. Thread B: 開始 drop 物件（設置 dead flag，調用 drop_fn）
3. Thread A: 調用 `inc_ref()` 並返回 Gc，**但物件已被釋放！** -> **Use-After-Free**

### 相關 Issue

- bug83: GcHandle resolve/clone TOCTOU (關於 root table 管理的 race)
- bug56: GcHandle::clone 缺少 dead 檢查 (已添加檢查但未解決原子性問題)
- bug62: GcHandle::resolve dropping_state 檢查遺漏 (已添加檢查但未解決原子性問題)

本 issue 是上述修復的**延伸問題**：即使添加了檢查，檢查與遞增之間仍存在 TOCTOU 競爭。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點 1：** `handles/cross_thread.rs:163-178` (resolve)

```rust
pub fn resolve(&self) -> Gc<T> {
    // ... 
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(!gc_box.is_under_construction(), ...);  // CHECK (line 165)
        assert!(!gc_box.has_dead_flag(), ...);          // CHECK (line 169)
        assert!(gc_box.dropping_state() == 0, ...);     // CHECK (line 173)
        gc_box.inc_ref();                               // USE - 競爭視窗!
        Gc::from_raw(...)
    }
}
```

**問題點 2：** `handles/cross_thread.rs:214-224` (try_resolve)

```rust
pub fn try_resolve(&self) -> Option<Gc<T>> {
    // ...
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        if gc_box.is_under_construction()    // CHECK
            || gc_box.has_dead_flag()       // CHECK
            || gc_box.dropping_state() != 0  // CHECK
        {
            return None;
        }
        gc_box.inc_ref();                   // USE - 競爭視窗!
        // ...
    }
}
```

**問題點 3：** `handles/cross_thread.rs:295-302` (clone)

```rust
fn clone(&self) -> Self {
    // ...
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.has_dead_flag()         // CHECK
                && gc_box.dropping_state() == 0  // CHECK
                && !gc_box.is_under_construction(),  // CHECK
            ...
        );
        gc_box.inc_ref();                   // USE - 競爭視窗!
    };
    // ...
}
```

**問題點 4：** `ptr.rs:437-456` (GcBoxWeakRef::upgrade)

```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    // ...
    unsafe {
        let gc_box = &*ptr.as_ptr();
        
        if gc_box.is_under_construction() { return None; }  // CHECK
        
        // BUGFIX: Check dropping_state BEFORE try_inc_ref_from_zero
        if gc_box.dropping_state() != 0 {  // CHECK
            return None;
        }
        
        // Try atomic transition from 0 to 1 (resurrection)
        if gc_box.try_inc_ref_from_zero() {  // 原子操作 (ref_count 0->1)
            return Some(...);
        }
        
        gc_box.inc_ref();  // USE - 競爭視窗!(ref_count > 0 時)
        // ...
    }
}
```

**問題點 5：** `ptr.rs:520-546` (GcBoxWeakRef::try_upgrade)

```rust
pub(crate) fn try_upgrade(&self) -> Option<Gc<T>> {
    // ...
    unsafe {
        let gc_box = &*ptr.as_ptr();
        
        if gc_box.is_under_construction() { return None; }  // CHECK
        
        if gc_box.is_dead_or_unrooted() { return None; }   // CHECK (包含 dead_flag)
        
        // ...
        
        gc_box.inc_ref();  // USE - 競爭視窗!
        // ...
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 TOCTOU 很難通過普通測試重現，需要精確的執行緒調度控制。PoC 理論上需要：

1. 建立 GcHandle
2. 在 Thread A 啟動 resolve()，在檢查後、inc_ref() 前暫停
3. Thread B 調用 dec_ref() 觸發 drop
4. Thread A 恢復並執行 inc_ref() - 此時物件已被釋放

```rust
// 理論 PoC - 需要精確時序控制
fn poc_toctou() {
    // 1. 建立 GcHandle
    let gc = Gc::new(Data);
    let handle = gc.cross_thread_handle();
    
    std::thread::spawn(move || {
        // 2. 等待 Thread A 進入 resolve
        thread::sleep(Duration::from_millis(10));
        
        // 3. 觸發 drop - 設置 dead_flag 並 drop 物件
        drop(handle);  // 這會調用 dec_ref，可能觸發 drop
    });
    
    std::thread::spawn(move || {
        // 4. Thread A 調用 resolve - 檢查通過但物件已死
        let resolved = handle.resolve();  // 可能 UAF!
    });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1: 使用原子檢查+遞增

將檢查與遞增合併為一個原子操作。對於 `ref_count` 從 N 到 N+1 的情況，可以使用 compare-exchange 迴圈：

```rust
// 理論上的原子遞增版本 (需要更複雜的設計)
fn atomic_check_and_inc(&self) -> Option<Gc<T>> {
    loop {
        let ref_count = self.ref_count.load(Ordering::Acquire);
        let weak_count = self.weak_count.load(Ordering::Acquire);
        
        // 檢查所有無效狀態
        if (weak_count & DEAD_FLAG) != 0 {
            return None;
        }
        if self.is_dropping.load(Ordering::Acquire) != 0 {
            return None;
        }
        
        // 嘗試 atomic 遞增
        match self.ref_count.compare_exchange_weak(
            ref_count, 
            ref_count + 1, 
            Ordering::AcqRel, 
            Ordering::Acquire
        ) {
            Ok(_) => return Some(...),
            Err(_) => continue, // ref_count 變了，重試
        }
    }
}
```

### 方案 2: 使用鎖 (簡單但有效能影響)

對於 GcHandle，可以使用 Mutex 保護整個 check+inc 操作：

```rust
// 概念性修復 - 需要修改 root table 結構
pub fn resolve(&self) -> Gc<T> {
    let tcb = self.origin_tcb.upgrade()
        .expect("origin thread terminated");
    
    // 獲取鎖保護整個操作
    let _guard = tcb.cross_thread_roots.lock().unwrap();
    
    // 再次檢查 handle 有效性
    if !roots.strong.contains_key(&self.handle_id) {
        panic!("handle unregistered");
    }
    
    unsafe {
        // 現在可以安全遞增
        let gc_box = &*self.ptr.as_ptr();
        // ... 檢查 ...
        gc_box.inc_ref();
    }
}
```

### 方案 3: 標記為已知問題，使用時注意

這是一個已知的 race condition，需要應用層配合：
- 不要在跨執行緒共享 handle 的同時進行 drop
- 使用強引用而不是 resolve

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**

這個 TOCTOU 是 GC 實現中的經典問題。在我的Chez Scheme GC 中，我們使用了「保護式遞增」模式：透過在 root table 中保持 entry 來防止物件在 check 和 use 之間被收集。rudo-gc 目前的實現缺少這種保護。

對於 GcHandle，解決方案應該是確保 handle 在 root table 中時，物件不會被收集，即使 ref_count 降到 0。這意味著需要修改 ref_count 的遞增/遞減邏輯。

**Rustacean (Soundness 觀點):**

這是一個明確的 UB 場景：程式可能在物件被 drop 後訪問其記憶體。雖然在 Rust 中很難直接利用（需要精確時序），但從記憶體安全角度這是一個 critical bug。

正確的修復需要將「檢查狀態」和「遞增 ref_count」合併為一個原子操作，或者確保在檢查和遞增期間物件受到保護（如持有鎖）。

**Geohot (Exploit 觀點):**

雖然理論上可以利用這個 race，但實際上非常困難：
1. 需要精確的執行緒調度
2. 需要知道何時物件會被 drop
3. 使用後的記憶體內容可能仍然有效（未覆蓋）

更實際的攻擊向量可能是：
- 透過造成 memory pressure 來加速記憶體重用
- 透過觀察 timing 來預測 drop 時機

這個 bug 更像是一個「理論上存在」的安全邊界問題，而非可直接利用的漏洞。

---

## 總結

此 bug 是 bug56/bug62 等修復的延伸問題。雖然已添加 `has_dead_flag()` 和 `dropping_state()` 檢查，但檢查與 `inc_ref()` 之間存在 TOCTOU 競爭視窗，可能導致 use-after-free。

修復需要將狀態檢查與 ref_count 遞增合併為原子操作，或使用其他同步機制保護整個 critical section.

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

1. **GcHandle** (resolve, try_resolve, clone): Already protected — holds `cross_thread_roots` lock during check+inc_ref; unregister does remove-then-dec_ref, so lock ordering prevents TOCTOU.

2. **GcBoxWeakRef** (upgrade, try_upgrade): Added `GcBox::try_inc_ref_if_nonzero()` — atomic fetch_update that only increments when `ref_count > 0`. Replaced `inc_ref()` with `try_inc_ref_if_nonzero()` in both upgrade paths; returns `None` when ref_count was 0 (object being dropped by another thread).
