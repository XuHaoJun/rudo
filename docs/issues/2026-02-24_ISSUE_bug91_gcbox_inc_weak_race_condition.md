# [Bug]: GcBox::inc_weak 使用 load+store 導致並發調用時 weak_count 丢失更新

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要多個線程同時克隆或下調用 Weak 引用 |
| **Severity (嚴重程度)** | `High` | 會導致 weak_count 不正確，可能造成記憶體洩露或重複釋放 |
| **Reproducibility (復現難度)** | `Medium` | 需要並發環境，但確定性高 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::inc_weak`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBox::inc_weak` 函數使用 `load() + store()` 模式來增加 weak_count，而不是使用原子的 `fetch_add()`。這在並發環境中會導致丢失更新（lost updates）。

### 預期行為 (Expected Behavior)
- 當多個線程同時調用 `inc_weak` 時，weak_count 應該正確增加相應的次數
- 例如：2 個線程同時調用，weak_count 應該增加 2

### 實際行為 (Actual Behavior)
- 由於使用 load+store 模式，會發生以下競態：
  1. Thread A: load weak_count = 10, 計算 new_count = 11
  2. Thread B: load weak_count = 10, 計算 new_count = 11  
  3. Thread A: store 11
  4. Thread B: store 11
- 結果：weak_count = 11（應該是 12！）
- 這導致 weak_count 永遠少於實際應該的值

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/ptr.rs:197-202`:

```rust
pub fn inc_weak(&self) {
    let current = self.weak_count.load(Ordering::Relaxed);
    let flags = current & Self::FLAGS_MASK;
    let count = current & !Self::FLAGS_MASK;
    let new_count = count.saturating_add(1);
    self.weak_count.store(flags | new_count, Ordering::Relaxed);  // BUG: 不是原子操作！
}
```

相比之下，`inc_ref` 正確地使用了 `fetch_update`:
```rust
pub fn inc_ref(&self) {
    self.ref_count
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            if count == usize::MAX {
                None
            } else {
                Some(count.saturating_add(1))
            }
        })
        .ok();
}
```

受影響的調用點（都可以並發調用）：
1. `GcBox::as_weak()` - 可從任何線程調用
2. `GcBoxWeakRef::clone()` - 可從任何線程調用
3. `Gc::downgrade()` - 可從任何線程調用
4. `Gc::weak_cross_thread_handle()` - 可從任何線程調用
5. `GcHandle::downgrade()` - 可從任何線程調用

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    
    // 創建多個 Weak 引用
    let num_threads = 4;
    let iterations_per_thread = 1000;
    
    // 使用 barrier 確保所有線程同時開始
    let barrier = Arc::new(thread::Barrier::new(num_threads));
    let weak_refs: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    
    let mut handles = vec![];
    
    for _ in 0..num_threads {
        let barrier = barrier.clone();
        let weak_refs = weak_refs.clone();
        
        handles.push(thread::spawn(move || {
            barrier.wait(); // 同步開始
            
            let mut local_weak = None;
            for i in 0..iterations_per_thread {
                if i % 10 == 0 {
                    // 每 10 次創建新的 weak 引用
                    local_weak = Some(gc.downgrade());
                }
                // 克隆現有的 weak 引用
                if let Some(ref w) = local_weak {
                    let _ = w.clone();
                    weak_refs.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    
    for h in handles {
        h.join().unwrap();
    }
    
    // 由於 bug，weak_count 會比預期少
    println!("Expected weak count: >= {}", num_threads * iterations_per_thread / 10);
    println!("Actual may be less due to race condition");
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `inc_weak` 改為使用原子的 `fetch_add`:

```rust
pub fn inc_weak(&self) {
    loop {
        let current = self.weak_count.load(Ordering::Relaxed);
        let flags = current & Self::FLAGS_MASK;
        let count = current & !Self::FLAGS_MASK;
        
        if count == usize::MAX - 1 {
            // Overflow protection - stay at MAX
            return;
        }
        
        let new_value = flags | (count + 1);
        if self.weak_count
            .compare_exchange_weak(current, new_value, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
    }
}
```

或者使用類似 `inc_ref` 的 fetch_update 模式：

```rust
pub fn inc_weak(&self) {
    self.weak_count
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            let flags = current & Self::FLAGS_MASK;
            let count = current & !Self::FLAGS_MASK;
            if count >= usize::MAX - 1 {
                None // Stay at MAX
            } else {
                Some(flags | (count + 1))
            }
        })
        .ok();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
weak_count 的準確性對於 GC 正確性至關重要。如果 weak_count 低於實際值，可能會導致物件被過早回收（記憶體不安全）。如果 weak_count 高於實際值，可能會導致記憶體洩露。這種丢失更新問題在高性能 GC 中是不可接受的。

**Rustacean (Soundness 觀點):**
這是 Rust 中的 data race（數據競態），屬於未定義行為。使用 `load() + store()` 模式而非原子操作會導致編譯器優化可能會產生錯誤的代碼。現代 Rust 應該避免這種模式。

**Geohot (Exploit 觀點):**
雖然這個 bug 主要影響內存管理的正確性，但在極端情況下：
1. 如果 weak_count 過低，物件可能被過早回收
2. 後續訪問已回收的記憶體可能導致 use-after-free
3. 攻擊者可能利用這種不確定性來繞過安全檢查

---

## ✅ 驗證記錄 (Verification Record)

**驗證日期:** 2026-02-24
**驗證人員:** opencode

### 驗證結果

已確認 bug 存在於 `crates/rudo-gc/src/ptr.rs:197-202`:

```rust
pub fn inc_weak(&self) {
    let current = self.weak_count.load(Ordering::Relaxed);
    let flags = current & Self::FLAGS_MASK;
    let count = current & !Self::FLAGS_MASK;
    let new_count = count.saturating_add(1);
    self.weak_count.store(flags | new_count, Ordering::Relaxed);  // BUG!
}
```

問題確認：
1. 使用 `load() + store()` 而非原子的 `fetch_add` 或 `compare_exchange`
2. 多線程並發調用時會發生丢失更新
3. `inc_ref` 正確使用了 `fetch_update`，但 `inc_weak` 沒有使用相同的模式

### 影響確認

此 bug 會導致：
- weak_count 低於實際值
- 物件可能被過早回收（如果其他線程也持有 strong reference）
- 記憶體安全問題潛在
- 與 `inc_ref` 的實現不一致

### 修復建議確認

建議使用 `fetch_update` 或 `compare_exchange_weak` 來確保原子性。
