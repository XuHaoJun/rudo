# [Bug]: clear_registers 未清除 RBX 導致虛假Roots

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | RBX 通常由編譯器管理，但理論上可能發生 |
| **Severity (嚴重程度)** | Medium | 可能導致對已釋放記憶體的虛假引用，造成記憶體腐敗 |
| **Reproducibility (復現難度)** | High | 需要特定寄存器分配模式，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `stack.rs` - `clear_registers()` 函數
- **OS / Architecture:** Linux x86_64, macOS x86_64, Windows x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`clear_registers()` 應該清除所有可能被用作GC Roots的callee-saved寄存器，特別是RBX、R12-R15。

### 實際行為 (Actual Behavior)
`clear_registers()` 只清除 R12-R15，但明確跳過 RBX（註釋說 "RBX is often reserved by LLVM"）。

有趣的是，`spill_registers_and_scan()` 會將 RBX 也壓到棧上並掃描作為潛在root。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `stack.rs` 中存在不一致：

**`spill_registers_and_scan` (lines 172-186):**
```rust
std::arch::asm!(
    "mov {0}, rbx",   // <-- RBX 被壓出
    "mov {1}, rbp",
    "mov {2}, r12",
    ...
);
```

**`clear_registers` (lines 264-282):**
```rust
std::arch::asm!(
    // "xor rbx, rbx",  // <-- RBX 未被清除！
    // "xor rbp, rbp", // Don't clear RBP, it might be frame pointer!
    "xor r12, r12",
    "xor r13, r13",
    ...
);
```

問題流程：
1. 假設某GC指標曾經在 RBX 中
2. `clear_registers()` 被調用（R12-R15 被清除，但 RBX 未被清除）
3. 後續 `spill_registers_and_scan()` 會掃描 RBX 作為潛在root
4. 如果那個舊的RBX值看起來像有效的GC指標，可能會被當作root

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上的PoC需要：
1. 分配一個GC對象
2. 確保指標被放入 RBX（可能需要內聯彙編或其他技巧）
3. 調用 `clear_registers()`
4. 觸發GC並觀察行為

**注意**：此問題難以穩定重現，因為編譯器可能將指標分配到其他寄存器。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

```rust
// 恢复清除 RBX
std::arch::asm!(
    "xor rbx, rbx",
    "xor r12, r12",
    "xor r13, r13",
    "xor r14, r14",
    "xor r15, r15",
    out("rbx") _,
    out("r12") _,
    ...
);
```

或者，如果 RBX 確實被LLVM保留為特殊用途，需要在 `spill_registers_and_scan` 中也跳過 RBX 以保持一致。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 Chez Scheme 中，我們確保所有可能持有指標的寄存器都被徹底清除。callee-saved 寄存器如 RBX 確實可能持有指標值。如果 `clear_registers` 只清除部分寄存器而 `spill_registers_and_scan` 掃描全部，會造成不一致。這可能導致虛假roots或漏掉真實roots。

**Rustacean (Soundness 觀點):**
`unsafe impl Sync` 的文檔注釋明確指出「currently accessed only from the GC thread」。如果將來有多線程並發訪問而沒有適當的同步機制，會導致UB。對於 `clear_registers`，問題在於假設「LLVM保留」是不穩固的——Rust編譯器不能保證RBX不被用作普通寄存器。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 RBX 的內容，結合conservative scan的行為，可能會利用虛假roots來防止特定記憶體區域被回收。這是一種記憶體腐敗攻擊的潛在向量。雖然難以利用，但理論上可行。