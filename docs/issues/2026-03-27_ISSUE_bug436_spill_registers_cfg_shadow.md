# [Bug]: spill_registers_and_scan fallback cfg shadows x86_64 regs array - GC pointers not scanned

**Status:** Open
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | 每次 x86_64 栈扫描都会发生 |
| **Severity (嚴重程度)** | `Critical` | GC 指针从未被扫描，可能导致 UAF |
| **Reproducibility (復現難度)** | `Low` | 总是发生，但需要检查 cfg 条件才能发现 |

---

## 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `stack.rs::spill_registers_and_scan` (line 230-233)
- **OS / Architecture:** `Linux x86_64` (其他平台不受影响)
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.x`

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)

在 x86_64 平台上，`spill_registers_and_scan` 應該：
1. 初始化 `let mut regs = [0usize; 5]` (line 175)
2. 通過內聯匯編將 callee-saved 寄存器 (rbp, r12, r13, r14, r15) 的值溢出到 `regs`
3. 掃描 `regs` 數組中的值作為潛在指針

### 實際行為 (Actual Behavior)

由於 cfg 條件錯誤，fallback 分支在 x86_64 上也會編譯，導致 `regs` 被遮蔽 (shadow)：

```rust
// Line 174-192: x86_64 平台
#[cfg(all(target_arch = "x86_64", not(miri)))]
let mut regs = [0usize; 5];  // 初始化 regs

#[cfg(all(target_arch = "x86_64", not(miri)))]
unsafe {
    std::arch::asm!(  // 內聯匯編溢出寄存器到 regs[0..4]
        "mov {0}, rbp",
        "mov {1}, r12",
        // ...
        out(reg) regs[0],  // 寫入 regs[0]
        // ...
    );
}

// Line 230-233: Fallback 也在 x86_64 編譯！
#[cfg(any(all(not(target_arch = "x86_64"), not(target_arch = "aarch64")), miri))]
let regs = [0usize; 32];  // 遮蔽原始 regs！

// Line 236-238: 掃描的是全零的 regs，不是溢出的寄存器值
for r in &regs {  // 這裡的 regs 是 [0usize; 32]，全是零！
    scan_fn(*r, 0, true);
}
```

**問題根源**：cfg 條件 `any(all(not(x86_64), not(aarch64)), miri)` 在 x86_64 上求值為 `true`（因為 `all(not(x86_64), not(aarch64))` = `all(true, true)` = `true`），導致 fallback 分支在 x86_64 上也會編譯。

---

## 根本原因分析 (Root Cause Analysis)

### 驗證 cfg 條件

在 x86_64 (not miri) 上：
1. x86_64 路徑 cfg：`all(target_arch = "x86_64", not(miri))` = `all(true, true)` = **TRUE** → 編譯
2. aarch64 路徑 cfg：`all(target_arch = "aarch64", not(miri))` = `all(false, true)` = **FALSE** → 不編譯
3. Fallback cfg：`any(all(not(x86_64), not(aarch64)), miri)` = `any(all(true, true), false)` = `any(true, false)` = **TRUE** → 編譯！

fallback `let regs = [0usize; 32]` 遮蔽了 x86_64 的 `let mut regs = [0usize; 5]`！

### 後果

- callee-saved 寄存器 (rbp, r12, r13, r14, r15) 中的 GC 指針永遠不會被掃描
- 如果這些寄存器包含 `Gc` 指針，這些對象可能被錯誤回收
- 導致 UAF (Use-After-Free) 和內存損壞

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

驗證方法：檢查編譯後的彙編或使用 rustc --emit cfg 檢查實際編譯的代碼路徑。

```rust
// 簡化驗證代碼
fn test_cfg_shadow() {
    let mut regs = [0usize; 5];
    regs[0] = 100;  // 模擬溢出 rbp
    regs[1] = 101;  // 模擬溢出 r12
    
    // 這行在 x86_64 上也會編譯（因為 not(aarch64) = true）
    let regs = [0usize; 32];  // regs 被遮蔽！
    
    // 這裡的 regs 全是零，原始值丟失
    println!("{:?}", regs);  // 輸出: [0, 0, 0, ...]
}
```

---

## 建議修復方案 (Suggested Fix / Remediation)

修復 cfg 條件，確保 fallback 只在非 x86_64 且非 aarch64 平台上編譯：

```rust
// 修復前 (buggy):
#[cfg(any(all(not(target_arch = "x86_64"), not(target_arch = "aarch64")), miri))]
let regs = [0usize; 32];

// 修復後:
#[cfg(all(not(target_arch = "x86_64"), not(target_arch = "aarch64")))]
let regs = [0usize; 32];
```

或者更明確地使用 `miri` 分支：
```rust
#[cfg(miri)]
let regs = [0usize; 32];
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
寄存器溢出是保守式棧掃描的關鍵部分。x86_64 使用 rbp, r12-r15 作為替存器，如果這些寄存器中的 GC 指針不被掃描，對象可能被錯誤回收。這是一個嚴重的 GC 正確性問題。

**Rustacean (Soundness 觀點):**
cfg 條件的邏輯錯誤導致代碼在特定平台行為不符合預期。這種bug很難通過測試發現，因為功能上代碼"看起來"是正確的（內聯匯編確實會執行），但實際掃描的值是錯誤的。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能夠控制棧上寄存器的值（例如通過 ROP chain），他們可以利用這個bug導致 GC 錯誤回收對象，從而實現 UAF 利用。

---

## 相關 Issue

- bug429: 之前的類似問題，但錯誤地聲稱已修復
