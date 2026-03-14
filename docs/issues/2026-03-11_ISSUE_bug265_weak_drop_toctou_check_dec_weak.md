# [Bug]: Weak::drop 與 WeakCrossThreadHandle::drop 存在 TOCTOU Race - 檢查與 dec_weak 之間的狀態變化

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在檢查通過後、dec_weak 調用前，另一執行緒改變物件狀態 |
| **Severity (嚴重程度)** | Low | 可能導致 weak_count 操作在不正確的物件狀態下執行 |
| **Reproducibility (復現難度)** | High | 需要精確的執行時序，很難穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::drop()` (ptr.rs), `WeakCrossThreadHandle::drop()` (cross_thread.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

在調用 `dec_weak()` 之前，應該確保物件狀態在整個操作過程中保持有效。即使檢查通過後，在實際調用 `dec_weak()` 之前，物件狀態也不應該發生變化。

### 實際行為 (Actual Behavior)

`Weak::drop` 和 `WeakCrossThreadHandle::drop` 存在經典的 TOCTOU (Time-Of-Check-Time-Of-Use) Race Condition：

1. 檢查 `has_dead_flag()`、`dropping_state()`、`is_under_construction()` 
2. 如果檢查通過，調用 `dec_weak()`
3. **Race Window**: 在步驟 1 和步驟 2 之間，另一個執行緒可能會改變物件狀態（例如開始 drop 物件）

### 程式碼位置

**ptr.rs:2263-2270 (Weak::drop)**:
```rust
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    return;
}
// TOCTOU window here - another thread could change state!
gc_box.dec_weak();
```

**cross_thread.rs:672-678 (WeakCrossThreadHandle::drop)**:
```rust
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    return;
}
// TOCTOU window here - another thread could change state!
gc_box.dec_weak();
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

這是一個經典的 TOCTOU Race Condition：

1. **Thread A** 調用 `Weak::drop` 或 `WeakCrossThreadHandle::drop`
2. 檢查 `has_dead_flag()`, `dropping_state()`, `is_under_construction()` - 全部返回 false
3. **Thread B** 開始 drop 同一個物件，設置 `dropping_state = 1` 或 `has_dead_flag`
4. **Thread A** 調用 `dec_weak()` - 此時物件正在被 drop！

雖然 `dec_weak()` 內部有檢查 `count == 0` 的保護，但問題在於：
- 我們在物件狀態可能改變的情況下仍然調用了 `dec_weak()`
- 這可能導致不一致的 weak_count 管理

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確的執行時序，非常難以穩定重現
// 概念驗證：
// 1. 創建一個帶有 Weak reference 的 Gc 物件
// 2. 使用並發原語（memory barrier, spin loop）嘗試在 Weak drop 檢查後、dec_weak 前觸發另一執行緒設置 dropping_state
// 3. 觀察 weak_count 是否被正確遞減
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有幾種可能的修復方案：

### 方案 1: 內聯檢查到 dec_weak 中（推薦）
修改 `GcBox::dec_weak()` 內部，在執行遞減前檢查這些狀態：

```rust
pub fn dec_weak(&self) -> bool {
    loop {
        let current = self.weak_count.load(Ordering::Relaxed);
        let flags = current & Self::FLAGS_MASK;
        let count = current & !Self::FLAGS_MASK;

        // Add state checks inside dec_weak
        if count == 0 {
            return false;
        }
        
        // Check if object is being dropped or dead
        if (flags & Self::DEAD_FLAG) != 0 {
            return false;
        }
        
        if (flags & Self::UNDER_CONSTRUCTION_FLAG) != 0 {
            return false;
        }
        
        // ... existing logic
    }
}
```

### 方案 2: 使用原子操作保護整個檢查+操作
將檢查和 dec_weak 合併為一個原子操作。

### 方案 3: 接受 Race（風險最低）
記錄這個 TOCTOU 為已知限制，因為 dec_weak 內部已經有 count == 0 檢查，實際影響較小。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 GC 系統中，weak reference 的遞減操作需要在整個過程中保持原子性。雖然目前的實現有基本保護，但 TOCTOU window 可能導致 weak_count 在物件生命周期結束後仍被操作。

**Rustacean (Soundness 觀點):**
這是一個確定的 Race Condition，儘管實際影響較小（dec_weak 內部有保護）。這不是 UB，但不符合最佳的並發設計實踐。

**Geohot (Exploit 攻擊觀點):**
在極端情況下，這個 TOCTOU 可能被利用來實現：
- 讓 weak_count 在物件死亡後仍保持 > 0
- 阻止物件被正確回收
- 但實際利用難度很高，需要精確的時序控制

---

## Resolution (2026-03-15)

**Fix applied:** Removed the early return on `has_dead_flag` / `dropping_state` / `is_under_construction` from both `Weak::drop` (ptr.rs) and `WeakCrossThreadHandle::drop` (cross_thread.rs).

**Rationale:** The pre-check was incorrect. When dropping a Weak, we must always decrement `weak_count` (when the slot is still allocated). Skipping `dec_weak` when the object is dead/dropping would prevent `weak_count` from reaching 0 and block reclamation. The `is_allocated` check (bug231/bug232) remains and protects against slot reuse (bug133). Eliminating the pre-check removes the TOCTOU window entirely — we always decrement, so there is no stale check.
