# [Bug]: GcRootSet::snapshot() 缺少 generation 檢查導致 slot 回收後被誤判為有效 root

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 發生於 GC 回收 slot 並重新分配給新物件後 |
| **Severity (嚴重程度)** | Critical | 可能導致 GC 標記錯誤物件，或新物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要控制 slot 回收和重新分配的時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRootSet::snapshot` (tokio/root.rs:126-142)
- **OS / Architecture:** Linux x86_64 (All)
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`GcRootSet::snapshot()` 方法使用 `find_gc_box_from_ptr` 驗證指標是否有效，但**沒有檢查 generation**。這意味著當 GC 回收一個 slot 並重新分配給新物件時，舊的 root 指針仍然會通過驗證。

### 預期行為 (Expected Behavior)
當 GC 做 snapshot 時，應該驗證每個 root 仍然指向**同一個**物件（相同的 generation），以防止 use-after-free 以及 slot 回收後被新物件佔用導致的錯誤標記。

### 實際行為 (Actual Behavior)
`GcRootSet::snapshot()` 返回所有在目前 heap 或 orphan heap 中有效的 registered 指針，**即使該 slot 已被回收並重新分配給完全不同物件**。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `tokio/root.rs:126-142` 中：

```rust
pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    let roots = self.roots.lock().unwrap();
    let valid_roots: Vec<usize> = roots
        .keys()
        .filter(|&&ptr| {
            // SAFETY: find_gc_box_from_ptr performs range and alignment checks.
            // If it returns Some, ptr is a valid GcBox.
            unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
        })
        .copied()
        .collect();
    // ...
}
```

問題：
1. `find_gc_box_from_ptr` 只檢查指標是否在有效範圍內、是否對齊、是否為有效 GcBox
2. **沒有檢查 generation** 以確認 slot 是否被回收並重新分配

對比 `AsyncHandle::get()` (handles/async.rs:647-656) 的正確實作：

```rust
let pre_generation = gc_box.generation();
// ... operations ...
// FIX bug453: If generation changed, undo the increment to prevent ref_count leak.
if pre_generation != gc_box.generation() {
    GcBox::undo_inc_ref(gc_box_ptr.cast_mut());
    panic!("AsyncHandle::get: slot was reused before value read (generation mismatch)");
}
```

`AsyncHandle::get()` 在操作前後都會檢查 generation，如果改變則 panic。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 在 tokio 環境中創建 `Gc` 物件並通過 `root_guard()` 註冊為 root
2. 觸發 GC 回收該物件（物件不再被引用）
3. 在同一個 slot 上分配新物件
4. 再次觸發 GC並呼叫 `snapshot()`
5. 觀察：舊的 root pointer 仍然被視為有效（但其實指向新物件）

**注意**：此 bug 需要多執行緒並髮用例才能穩定復現，單執行緒可能無法可靠觸發。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcRootSet::snapshot()` 中新增 generation 檢查：

1. 獲取 `find_gc_box_from_ptr` 返回的 GcBox
2. 從 GcBox 中讀取 generation（或許需要從 HashMap 中存儲原本的 generation）
3. 對比存儲的 generation 與當前 GcBox 的 generation
4. 如果 generation 不匹配，視為無效 root

另一方案：修改 HashMap 儲存結構為 `HashMap<usize, Generation>` 或類似結構，在 snapshot 時比對 generation。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- GcRootSet 是 tokio 整合的關鍵元件，負責追蹤跨 async task 的 GC roots
- 如果 GC 漏標 root，會導致 live 物件被回收，這是嚴重的正確性問題
- generation 機制是偵測 slot 回收後重新分配的核心手段，必須在所有路徑上实施
- 此問題與 bug327、bug142、bug151 相關但不同 - 那些是 dirty flag 和 single heap 過濾的問題

**Rustacean (Soundness 觀點):**
- `find_gc_box_from_ptr` 的 safety comment 只說「performs range and alignment checks」，但沒有提到 generation
- 這可能導致不一致假設：某些呼叫者可能認為 `Some` 返回值表示「同一個物件」
- 建議在 `find_gc_box_from_ptr` 的文件中明確說明其不檢查 generation

**Geohot (Exploit 觀點):**
- 攻擊者可以通過控制分配模式，強制 slot 回收並重新分配
- 如果攻擊者能在 GC snapshot 前噴灑新物件到該 slot，就能讓 GC 錯誤標記攻擊者控制的物件
- 這可能導致物件被錯誤保留（由於舊 root 指向新物件），或新物件被錯誤回收
- 這是一個 memory corruption 潛在入口點