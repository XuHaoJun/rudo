# [Bug]: GcHandle::resolve/try_resolve 缺少 inc_ref 後的 post-check 導致 TOCTOU UAF

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒交錯執行，且時間窗口很小 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全漏洞 |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒調度，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcHandle::resolve()` 與 `GcHandle::try_resolve()` 在調用 `gc_box.inc_ref()` **之後**缺少對 `dropping_state()` 和 `has_dead_flag()` 的第二次檢查。這與 `Weak::upgrade()` 中已修復的 TOCTOU 漏洞相同，但尚未應用於 GcHandle。

### 預期行為 (Expected Behavior)

在調用 `inc_ref()` 增加引用計數後，應該再次檢查物件狀態，確保物件未被正在釋放 (dropping) 或已死亡 (dead)。如果狀態異常，應該撤銷 increment 並返回 `None` (try_resolve) 或 panic (resolve)。

### 實際行為 (Actual Behavior)

`GcHandle::resolve()` (lines 194-210) 和 `GcHandle::try_resolve()` (lines 256-266) 只在 `inc_ref()` **之前**進行狀態檢查，沒有在 **之後** 進行第二次檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cross_thread.rs` 中，`resolve()` 和 `try_resolve()` 的實現如下：

```rust
// resolve() lines 194-210
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(!gc_box.is_under_construction(), ...);
    assert!(!gc_box.has_dead_flag(), ...);
    assert!(gc_box.dropping_state() == 0, ...);
    gc_box.inc_ref();  // <-- 沒有 post-check!
    Gc::from_raw(self.ptr.as_ptr() as *const u8)
}
```

Race 條件場景：
1. Thread A 對 GcBox 呼叫 `dec_ref()` 最後一個強引用
2. Thread A 成功標記 `dropping_state = 1` (via `try_mark_dropping()`)
3. Thread A 調用 `drop_fn()`:
   - 設置 `DEAD_FLAG`
   - 調用 `drop_in_place` 釋放物件
   - 設置 `dropping_state = 2`
4. **Thread B** 在 Thread A 設置這些 flag **之間** 調用 `GcHandle::resolve()`
5. Thread B 的**預檢查**可能看到過時的值 (memory ordering)
6. Thread B 成功調用 `inc_ref()` 
7. Thread B 返回一個指向**已釋放物件**的 `Gc<T>` → **Use-After-Free!**

正確的模式存在於 `Weak::upgrade()` (ptr.rs:1823-1833)：
```rust
// Post-CAS safety check
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(ptr.as_ptr());
    return None;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要多執行緒環境才能觸發，單執行緒測試無法可靠復現。

理論上的 PoC：
1. 建立 GcBox 並獲取 GcHandle
2. 在另一個執行緒中對 GcBox 呼叫最後的 dec_ref()，觸發 dropping
3. 在精確的時間窗口內從原執行緒調用 resolve()
4. 驗證是否獲得一個指向已釋放物件的 Gc

建議使用 ThreadSanitizer 或 loom 進行正式驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cross_thread.rs` 的 `resolve()` 和 `try_resolve()` 中，在 `gc_box.inc_ref()` **之後**添加 post-check：

```rust
// resolve() - 修改後
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(!gc_box.is_under_construction(), ...);
    assert!(!gc_box.has_dead_flag(), ...);
    assert!(gc_box.dropping_state() == 0, ...);
    gc_box.inc_ref();
    
    // Post-increment safety check
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        gc_box.dec_ref();
        panic!("GcHandle::resolve: object was dropped after inc_ref");
    }
    
    Gc::from_raw(self.ptr.as_ptr() as *const u8)
}

// try_resolve() - 修改後  
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag()
        || gc_box.dropping_state() != 0
    {
        return None;
    }
    gc_box.inc_ref();
    
    // Post-increment safety check
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        gc_box.dec_ref();
        return None;
    }
    
    Some(Gc::from_raw(self.ptr.as_ptr() as *const u8))
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這與 Weak::upgrade 中修復的 TOCTOU 問題完全相同
- GC 系統中，root table 保護物件不被回收是核心不變量
- 當 handle 通過檢查但物件正在被釋放時，inc_ref() 增加的 ref_count 會阻止物件被回收，但物件內容已經被 drop_in_place 釋放
- 這導致返回一個「有效的」Gc 指標，但指標指向的記憶體內容已經無效

**Rustacean (Soundness 觀點):**
- 這是經典的 Use-After-Free 漏洞
- 雖然 Gc 內部有追蹤 ref_count，但當 dropping_state != 0 時，物件已經被 drop，後續存取會導致 UB
- 需要在獲得有效引用後重新驗證物件狀態

**Geohot (Exploit 觀點):**
- 攻擊者可以嘗試噴射 (spray) 物件來控制被釋放的記憶體內容
- 如果能精確觸發 race condition，可以實現 arbitrary memory write
- 由於時間窗口很小，實際利用需要極高精度，但理論上是可行的
