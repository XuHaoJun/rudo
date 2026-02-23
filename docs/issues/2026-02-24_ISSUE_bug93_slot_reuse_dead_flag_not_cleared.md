# [Bug]: Slot Reuse 時未清除 DEAD_FLAG 導致新物件被錯誤標記為死亡

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 每當 GC 回收物件並重用其 slot 時都會觸發 |
| **Severity (嚴重程度)** | Critical | 導致新物件被誤判為死亡，影響 Weak 升級、跨執行緒 Handle 解析 |
| **Reproducibility (復現難度)** | Medium | 需要設計能觸發 slot 重用並檢查 has_dead_flag() 的 PoC |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap`, `try_pop_from_page`, `tlab_alloc`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

當 GC 回收物件並將其 slot 加入空閒串列後，日後該 slot 被重用於新物件時，**DEAD_FLAG 未被清除**。這導致 `has_dead_flag()` 對新物件錯誤返回 `true`。

### 預期行為 (Expected Behavior)
當 slot 被重用時，GcBox 的所有 flags（包括 DEAD_FLAG）應該被清除，確保新物件是一個乾淨的「live」物件狀態。

### 實際行為 (Actual Behavior)
- 在 `try_pop_from_page()` (heap.rs:2131-2142) 和 TLAB allocation (heap.rs:1292) 中，只呼叫了 `set_allocated()` 更新 bitmap
- Page 層級的 `all_dead()` 和 `dead_count` 被清除
- **但 GcBox 層級的 DEAD_FLAG 未被清除**

相比之下，`clear_gen_old()` 在 slot 加入空閒串列時被正確呼叫 (heap.rs:2556)，但 DEAD_FLAG 沒有類似的清除機制。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **GcBox 的 DEAD_FLAG** 儲存於 `weak_count` 的高位元 (ptr.rs:52)
2. **`set_dead()` 方法** 用於設定 DEAD_FLAG (ptr.rs:294-295)
3. **缺少 `clear_dead()` 方法**：存在 `clear_gen_old()` (ptr.rs:334-337) 清除 GEN_OLD_FLAG，但無對應的 `clear_dead()`
4. **Slot 重用時**:
   - `try_pop_from_page()` (heap.rs:2131-2142): 只呼叫 `set_allocated()`，未清除 DEAD_FLAG
   - TLAB allocation (heap.rs:1292): 同樣只呼叫 `set_allocated()`
5. **影響範圍**:
   - `has_dead_flag()` 對新物件錯誤返回 `true`
   - 跨執行緒 Handle 解析失敗 (handles/cross_thread.rs:169, 217, 252, 297)
   - Weak::upgrade() 失敗 (ptr.rs:433, 1083, 1100)
   - Gc::clone() / Gc::downgrade() 檢查失敗

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要設計 PoC 驗證 slot 重用後 has_dead_flag() 狀態
// 1. 配置 GC 使其回收物件並將 slot 加入空閒串列
// 2. 配置 GC 重新分配該 slot
// 3. 檢查新物件的 has_dead_flag() 是否為 true (預期為 false)
```

**注意**：根據 AGENTS.md 的驗證指南，需使用 **minor GC** (`collect()`) 而非 `collect_full()` 來測試 barrier 相關問題。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **新增 `GcBox::clear_dead()` 方法** (ptr.rs):
   ```rust
   pub(crate) fn clear_dead(&self) {
       self.weak_count
           .fetch_and(!Self::DEAD_FLAG, Ordering::Relaxed);
   }
   ```

2. **在 `try_pop_from_page()` 中呼叫** (heap.rs:2131-2142):
   - 在 `set_allocated()` 後，檢查並清除 DEAD_FLAG：
   ```rust
   // 清除 DEAD_FLAG，確保重用 slot 是乾淨的 live 物件
   unsafe { (*gc_box_ptr).clear_dead() };
   ```

3. **在 TLAB allocation 中呼叫** (heap.rs:1292):
   - 同樣需要清除 DEAD_FLAG

4. **或者更徹底地**：在 slot 從空閒串列彈出時清除所有 flags (類似 clear_gen_old 的模式)

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 在 Chez Scheme 的 GC 中，slot 重用時會確保所有元資料被清除
- 這是一個經典的「stale metadata」問題 - slot 重用時必須有乾淨的初始狀態
- Page 層級的 `all_dead` 和 `dead_count` 已經正確清除，但 GcBox 層級的 flag 被遺漏

**Rustacean (Soundness 觀點):**
- 這不是傳統意義的 UB，但可能導致邏輯錯誤
- 新物件的 `has_dead_flag()` 返回 true 會導致後續操作失敗（如 Weak::upgrade）
- 需要確保 `clear_dead()` 使用適當的 memory ordering

**Geohot (Exploit 觀點):**
- 如果攻擊者能控制 GC 時序，可能利用此 bug 導致物件被錯誤回收
- Weak reference upgrade 失敗可能導致預期外的 NULL 處理
- 這更像是一個 reliability bug 而非安全漏洞

