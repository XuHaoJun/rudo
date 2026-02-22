# [Bug]: GcBoxWeakRef::upgrade ref_count leak due to TOCTOU between try_inc_ref_from_zero and dropping_state check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：weak upgrade 與 object dropping 並行 |
| **Severity (嚴重程度)** | High | ref_count leak 導致 object 無法被回收，長期導致 memory leak |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade` (ptr.rs:422-456)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`GcBoxWeakRef::upgrade()` 方法存在 TOCTOU (Time-of-Check-Time-of-Use) race condition，導致 ref_count leak。

### 預期行為
- 當 object 正在被 dropping 時，`upgrade()` 應返回 `None`，且不應修改 ref_count
- 或者，如果 upgrade 成功，object 應該保持 alive 狀態

### 實際行為
1. `try_inc_ref_from_zero()` 成功將 ref_count 從 0 增加到 1
2. 隨後檢查 `dropping_state() != 0` 發現 object 正在 dropping
3. 返回 `None` - 但 ref_count 已經是 1，造成 leak

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:437-448`:
```rust
// Try atomic transition from 0 to 1 (resurrection)
if gc_box.try_inc_ref_from_zero() {
    return Some(Gc { ... });  // 這裡 ref_count 已變成 1
}

// Object is being dropped - do not allow new strong refs (UAF prevention)
if gc_box.dropping_state() != 0 {
    return None;  // BUG: ref_count 已經是 1，但被丟棄了！
}
```

`try_inc_ref_from_zero()` 只檢查:
- `DEAD_FLAG`
- `ref_count != 0`

但**不檢查** `dropping_state()`。

相比之下，`try_upgrade()` (ptr.rs:515-526) 正確地先檢查 `dropping_state()`:
```rust
if gc_box.dropping_state() != 0 {
    return None;
}
// Try atomic transition from 0 to 1
if gc_box.try_inc_ref_from_zero() { ... }
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發場景：
1. 建立一個即將被 drop 的 Gc object (ref_count = 1)
2. 同時從另一個 thread 嘗試 weak upgrade
3. 時序：upgrade thread 先通過 try_inc_ref_from_zero，然後 dropping thread 設置 dropping_state

由於時序極難控制，建議使用 model checker (如 loom) 驗證。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `dropping_state()` 檢查移到 `try_inc_ref_from_zero()` 之前，與 `try_upgrade()` 保持一致：

```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire).as_option()?;

    unsafe {
        let gc_box = &*ptr.as_ptr();

        if gc_box.is_under_construction() {
            return None;
        }

        // If DEAD_FLAG is set, value has been dropped - cannot resurrect
        if gc_box.has_dead_flag() {
            return None;
        }

        // BUGFIX: Check dropping_state BEFORE try_inc_ref_from_zero
        // Object is being dropped - do not allow new strong refs (UAF prevention)
        if gc_box.dropping_state() != 0 {
            return None;
        }

        // Try atomic transition from 0 to 1 (resurrection)
        if gc_box.try_inc_ref_from_zero() {
            return Some(Gc {
                ptr: AtomicNullable::new(ptr),
                _marker: PhantomData,
            });
        }

        // If we reach here, ref_count > 0, increment normally
        gc_box.inc_ref();
        Some(Gc {
            ptr: AtomicNullable::new(ptr),
            _marker: PhantomData,
        })
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
升級過程中的 TOCTOU 問題在 GC 實現中很常見。正確的做法是在原子操作之前檢查所有無效狀態（dead、dropping），確保狀態一致。`try_upgrade()` 的實現是正確的範例。

**Rustacean (Soundness 觀點):**
這是一個內存安全問題：ref_count leak 可能導致 object 永遠無法被回收，長期下來造成 memory leak。在極端情況下可能導致 out-of-memory。

**Geohot (Exploit 觀點):**
這種 TOCTOU race 很難利用，因為需要精確的時序控制。但理論上可以通過精確控制線程調度來觸發，導致目標程式 memory leak。
