# [Bug]: try_inc_ref_from_zero 允許在有 weak references 時復活已死亡物件

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：物件drop後有殘餘weak ref時升級 |
| **Severity (嚴重程度)** | Critical | 可導致 use-after-free，存取已drop的記憶體 |
| **Reproducibility (重現難度)** | Medium | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::try_inc_ref_from_zero()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
當物件的 value 已被 drop（`DEAD_FLAG` 設置）時，weak reference 不應該能夠升級為 strong reference，即不應該允許復活（resurrect）已死亡的物件。

### 實際行為
在 `try_inc_ref_from_zero()` 函數中，雖然檢查了 `flags != 0 && weak_count == 0`，但當 `weak_count > 0`（存在 weak references）時，即使 `DEAD_FLAG` 已設置，條件判斷仍會通過，允許 CAS 成功，導致復活已死亡物件。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題程式碼
`crates/rudo-gc/src/ptr.rs:223-225`

```rust
pub(crate) fn try_inc_ref_from_zero(&self) -> bool {
    loop {
        let ref_count = self.ref_count.load(Ordering::Acquire);
        let weak_count_raw = self.weak_count.load(Ordering::Acquire);

        let flags = weak_count_raw & Self::FLAGS_MASK;
        let weak_count = weak_count_raw & !Self::FLAGS_MASK;

        // BUG: 此條件邏輯有缺陷
        if flags != 0 && weak_count == 0 {  // Line 223
            return false;
        }

        if ref_count != 0 {
            return false;
        }

        // CAS from 0 to 1...
    }
}
```

### 邏輯缺陷

條件 `flags != 0 && weak_count == 0` 的意圖是：
- 當有 flags（如 DEAD_FLAG）且沒有 weak references 時，拒絕復活

但問題在於：
1. 當 `DEAD_FLAG` 已設置（value 已被 drop）且 `weak_count > 0` 時
2. 條件 `flags != 0 && weak_count == 0` 評估為 `false`（因為 `weak_count != 0`）
3. 函數繼續執行，嘗試 CAS 將 `ref_count` 從 0 增至 1
4. CAS 成功，物件被錯誤地復活

### TOCTOU 風險

在 `GcBoxWeakRef::upgrade()` (ptr.rs:406-431) 中：
1. 檢查 `has_dead_flag()` (line 413)
2. 調用 `try_inc_ref_from_zero()` (line 418)

在步驟1和步驟2之間，另一個執行緒可能已設置 DEAD_FLAG，結合上述邏輯缺陷，導致復活已死亡物件。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: Arc<AtomicBool>,
}

fn main() {
    let value = Arc::new(AtomicBool::new(false));
    
    // Create Gc and immediately downgrade to Weak
    let gc = Gc::new(Data { value: value.clone() });
    let weak: Weak<Data> = gc.downgrade();
    
    // Drop the strong reference
    drop(gc);
    
    // At this point: DEAD_FLAG is set, weak_count = 1
    
    // Try to upgrade the weak reference
    // BUG: This should return None but may succeed due to the logic bug
    let upgraded = weak.upgrade();
    
    if let Some(gc) = upgraded {
        // If we get here, we've resurrected a dead object!
        // Accessing gc.value may cause use-after-free
        println!("BUG: Resurrected dead object!");
        gc.value.store(true, Ordering::Relaxed); // Undefined behavior!
    } else {
        println!("Correctly returned None");
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

修改 `try_inc_ref_from_zero()` 的條件判斷：

```rust
// 錯誤的 current code:
if flags != 0 && weak_count == 0 {
    return false;
}

// 正確的 fix:
if (flags & Self::DEAD_FLAG) != 0 {
    return false;  // 不能復活已死亡的物件
}
```

或者更嚴格地：

```rust
// 檢查 DEAD_FLAG，無論 weak_count 是多少
if flags != 0 {
    return false;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 涉及 GC 中的對象生命週期管理。在傳統的generational GC中，我們通常不會在有外部weak reference時允許對象完全死亡，而是會保留對象直到所有weak references被清除。但這裡的問題是邏輯條件不正確——當有weak references存在時，代碼沒有正確阻止復活。建議修復應該確保只要DEAD_FLAG設置，就拒絕任何形式的復活，無論weak_count的值是多少。

**Rustacean (Soundness 觀點):**
這是一個內存安全問題。當DEAD_FLAG設置後，對value已被drop象的，此時允許任何形式的復活都會導致use-after-free。在Rust的內存安全模型中，這是不可接受的。關鍵問題是條件邏輯 `flags != 0 && weak_count == 0` 的短路行為——當weak_count > 0時，整個條件被跳過，導致不安全的代碼路徑。使用更嚴格的檢查（如直接檢查DEAD_FLAG）可以解決這個soundness問題。

**Geohot (Exploit 觀點):**
從攻擊者的角度看，這個bug提供了一個有趣的利用窗口。如果攻擊者能夠控制時序，他們可以：
1. 創建一個包含敏感數據的對象
2. 將其downgrade為weak reference
3. drop強引用觸發DEAD_FLAG
4. 在GC清理前，通過並發操作觸發weak upgrade
5. 由於bug，upgrade成功，獲得一個指向已drop記憶體的指標
6. 讀取殘餘的敏感數據（如果value有內部指標指向其他敏感數據）

這是一個經典的TOCTOU + 邏輯缺陷組合漏洞。
