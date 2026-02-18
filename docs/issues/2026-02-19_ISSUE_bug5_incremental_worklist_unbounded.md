# [Bug]: Incremental Marking 增量標記階段 Overflow 時的 Worklist 無界成長

## 📊 威脅模型評估 (Threat Model Assessment)

| 指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當標記 worklist overflow 時觸發 |
| **Severity (嚴重程度)** | High | 可能導致記憶體耗盡 |
| **Reproducibility (復現難度)** | Medium | 需要大量指標結構觸發 overflow |

---

## 🧩 受影響的組件與環境
- **Component:** `IncrementalMarkState`, `mark_slice`, `FallbackReason::WorklistUnbounded`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

在增量標記期間，當 worklist（工作列表）溢出時，系統會觸發 fallback 到 STW（Stop-The-World）模式。然而，fallback 機制存在以下問題：

1. 當 `FallbackReason::WorklistUnbounded` 觸發時，表示 worklist 無界成長
2. 切換到 STW 後，需要處理累積的大量 worklist 項目
3. 如果 worklist 過大，可能導致：
   - 標記階段時間過長（失去增量標記的目的）
   - 記憶體耗盡

### 預期行為
- 當 worklist 過大時，應該有合理的機制處理
- fallback 應該能夠快速完成標記

### 實際行為
- Worklist 無界成長
- Fallback 觸發後，需要處理大量待標記物件
- 可能導致長時間 STW暫停

---

## 🔬 根本原因分析

在 `gc/incremental.rs` 中：

```rust
pub fn mark_slice(
    // ...
) -> MarkSliceResult {
    // ...
    if worklist.len() > config.max_worklist_size {
        return MarkSliceResult::Fallback {
            reason: FallbackReason::WorklistUnbounded,
        };
    }
    // ...
}
```

問題：
1. Worklist 使用 `SegQueue<*const GcBox<()>>`（非同步隊列）
2. 當標記進行時，對象可能被多次加入 worklist（因為多個指標指向同一对象）
3. 沒有有效的去重機制
4. `FallbackReason::WorklistUnbounded` 觸發時，累積的 worklist 項目可能已經很大

---

## 💣 PoC

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};

#[derive(Trace)]
struct Node {
    children: GcCell<Vec<Gc<Node>>>,
}

fn main() {
    // 創建深度指標圖
    let mut root = Gc::new(Node { 
        children: GcCell::new(Vec::new()) 
    });
    
    // 創建大量交叉引用的節點
    for i in 0..100000 {
        let node = Gc::new(Node {
            children: GcCell::new(Vec::new()),
        });
        
        // 每個節點引用大量其他節點
        let mut children = node.children.borrow_mut();
        for _ in 0..100 {
            children.push(root.clone());
        }
        
        let mut root_children = root.children.borrow_mut();
        root_children.push(node);
    }
    
    // 觸發增量標記
    // 這會導致 worklist 快速增長並溢出
    collect_full();
}
```

---

## 🛠️ 建議修復方案

### 方案 1：實現 Worklist 去重
在加入 worklist 前檢查是否已經標記過：

```rust
fn push_to_worklist(&self, obj: *const GcBox<()>) {
    if !self.is_marked(obj) {
        self.worklist.push(obj);
    }
}
```

### 方案 2：限制 Worklist 大小並使用 BitSet 追蹤
使用 mark bitmap 來追蹤已處理過的對象，避免重複處理：

```rust
pub fn mark_slice(&mut self) {
    while let Some(obj) = self.worklist.pop() {
        if self.is_marked(obj) {
            continue; // 跳過已標記的對象
        }
        // 標記並追蹤引用
    }
}
```

### 方案 3：改進 Fallback 邏輯
在 fallback 時，不僅僅切換到 STW，還應該：

1. 處理當前 worklist
2. 記錄剩餘需要標記的對象
3. 在下一個 slice 繼續處理

---

## 🗣️ 內部討論

**R. Kent Dybvig:**
此問題反映了增量 GC 的經典挑戰：如何在增量性和完整性之間取得平衡。建議使用「灰色工作清單」機制，確保每個對象只被處理一次。

**Rustacean:**
這是記憶體效率問題，而非安全性問題。但長時間的 STW 暫停會影響程式的回應性。

**Geohot:**
雖然不是直接的安全問題，但攻擊者可以通過構造特殊的指標結構來觸發過長的 STW 暫停，實現 DoS 攻擊。
