# [Bug]: Ephemeron GcCapture TOCTOU 導致 Key GC 指標可能被遺漏捕獲

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要 Ephemeron 的 key 在捕獲過程中死亡 |
| **Severity (嚴重程度)** | Medium | 可能導致 Key 的 GC 指標未被正確捕獲，影響 SATB 正確性 |
| **Reproducibility (復現難度)** | High | 需精確時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture for Ephemeron`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當 `Ephemeron` 的 key 處於 alive 狀態時，`GcCapture::capture_gc_ptrs_into()` 應該捕獲：
1. Value 的 GC 指標
2. Key 的 GC 指標

兩者都應該被捕獲，以確保 SATB barrier 的正確性。

### 實際行為 (Actual Behavior)

在 `ptr.rs` 的 `GcCapture for Ephemeron` 實現中存在 TOCTOU (Time-of-check-Time-of-use) 漏洞：

```rust
// ptr.rs:2293-2303
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // Ephemeron semantics: only capture when key is alive (matches Trace).
    // When key is dead, value can be collected; capturing would incorrectly retain it.
    if self.is_key_alive() {              // <-- Step 1: 檢查 key 是否 alive
        self.value.capture_gc_ptrs_into(ptrs);  // <-- Step 2: 捕獲 value 指標
        if let Some(key_gc) = self.key.try_upgrade() {  // <-- Step 3: 嘗試升級 key
            key_gc.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

問題在於：
1. Step 1 檢查 `is_key_alive()` 返回 true（key 活著）
2. Step 2 捕獲 value 的 GC 指標
3. **在 Step 1 和 Step 3 之間**，另一個執行緒可能會丟棄 key 的最後一個強引用
4. Step 3 `try_upgrade()` 返回 None（key 已死亡）
5. **Key 的 GC 指標未被捕獲！**

### 根本原因

`is_key_alive()` 和 `try_upgrade()` 不是原子操作。在多執行緒環境下，key 可能會在檢查後、捕獲前死亡。

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU (Time-of-check-Time-of-use) 漏洞：

1. **檢查點** (`is_key_alive()`): 驗證 key 處於 alive 狀態
2. **使用點** (`try_upgrade()`): 嘗試獲取 key 的強引用

兩個操作之間沒有同步機制，導致狀態可能改變。

對比 `Trace for Ephemeron` 的實現（ptr.rs:2252-2261）：
```rust
fn trace(&self, visitor: &mut impl Visitor) {
    if self.is_key_alive() {
        visitor.visit(&self.value);
    }
}
```

這裡沒有問題，因為只需要檢查 value 是否需要追蹤。但 `GcCapture` 需要捕獲**所有** GC 指標，包括 key 的指標。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, cell::GcCapture};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

#[derive(Trace)]
struct KeyData {
    value: i32,
}

#[derive(Trace)]
struct ValueData {
    data: i32,
}

fn main() {
    // 1. 創建一個 Ephemeron，key 和 value 都在 GC heap 中
    let key = Gc::new(KeyData { value: 42 });
    let value = Gc::new(ValueData { data: 100 });
    let ephemeron = Gc::new(rudo_gc::Ephemeron::new(&key, value));
    
    // 2. 在一個執行緒中持續調用 capture_gc_ptrs_into
    // 3. 在另一個執行緒中，同時 drop 最後的 key 引用
    
    // 問題：如果 key 在 is_key_alive() 和 try_upgrade() 之間死亡，
    // key 的 GC 指標將不會被捕獲
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩種修復方案：

### 方案 1: 先捕獲 Key，再檢查（推薦）

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // 先嘗試升級 key（原子操作）
    if let Some(key_gc) = self.key.try_upgrade() {
        // Key 活著，捕獲 key 的指標
        key_gc.capture_gc_ptrs_into(ptrs);
        
        // 捕獲 value 的指標
        self.value.capture_gc_ptrs_into(ptrs);
    }
    // 如果 key 已經死亡，什麼都不做（符合 ephemeron 語義）
}
```

### 方案 2: 使用原子檢查

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // 使用原子操作同時檢查 key 狀態並準備捕獲
    // 這需要修改底層 API 來支持原子升級+捕獲
}
```

**推薦方案 1**，因為：
1. 語義正確：如果 key 活著，捕獲 key 和 value；如果 key 死亡，什麼都不做
2. 簡單明確
3. 與 `Trace` 的實現一致（只是 `Trace` 只處理 value）

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是典型的 TOCTOU 漏洞。在 GC 的 SATB barrier 中，我們需要在同一個原子操作中確定 key 的狀態並捕獲其指標。當前實現允許 key 在檢查和使用之間死亡，導致指標遺漏。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB，但可能導致記憶體管理錯誤。如果 key 的指標未被正確捕獲，GC 可能會錯誤地回收這些物件，導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
在極端情況下，這可能被利用來觸發 UAF：如果攻擊者能夠控制 key 的死亡時機，可能導致 GC 錯誤地回收 key 物件，雖然這需要精確的時序控制。

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
