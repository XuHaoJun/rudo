---
name: start-bug-hunt
description: Structured bug hunting for rudo-gc using multi-perspective analysis. Simulates R. Kent Dybvig (GC/memory), Rustacean (soundness/UB), and George Hotz (exploits/race conditions). Use when hunting bugs, debugging GC issues, when the user invokes /start-bug-hunt, or asks to find program bugs (尋找程式 bug).
---

# Start Bug Hunt

Structured bug hunting workflow with parallel-world expert collaboration. Output goes to `docs/issues/`.

## Preconditions

- **Assume all tests pass.** Do not run `./test.sh` or `cargo test` as part of the bug hunt.
- **Read existing tests** in `crates/rudo-gc/tests/` to understand coverage; use this to identify untested or under-tested code paths.

## Personas

| Expert | Perspective |
| :--- | :--- |
| **R. Kent Dybvig** | Chez Scheme author; GC, memory layout, and allocator precision. |
| **Rustacean** (Rust Leadership Council) | Memory safety, soundness, UB sensitivity, Send/Sync correctness. |
| **George Hotz** (Geohot) | Exploit mindset; system boundaries, race conditions, fragile low-level mechanisms. |

## Workflow

### Step 1: Find the Bug

- Analyze code, reproduce failure, or trace from symptoms.
- Formulate clear expected vs actual behavior.
- **CRITICAL STOPPING CONDITION**: If no bug is found during the analysis, **STOP THE WORKFLOW IMMEDIATELY**. Do not proceed to the next steps, do not create any issues, and report directly to the user that no bugs were found.

### Step 2: Check for Duplicates

- Search `docs/issues/` and `docs/history-issues/` for related issues.
- **If duplicate found**: Update existing issue with new findings (PoC, root cause, fix ideas). Do not create a new file.

### Step 3: Create Issue File (if no duplicate)

- Ensure `docs/issues/` exists.
- Create file: `docs/issues/YYYY-MM-DD_ISSUES_<bug-name>.md` (use today's date).
- Use the template in [ISSUE_TEMPLATE.md](ISSUE_TEMPLATE.md).

## Output Format

Use the full template in [ISSUE_TEMPLATE.md](ISSUE_TEMPLATE.md). Essential sections:

1. **Threat Model** – Likelihood, Severity, Reproducibility table  
2. **Affected Component** – Component, OS, Rust/rudo-gc versions  
3. **Description** – Expected vs Actual behavior  
4. **Root Cause Analysis** – Technical details  
5. **PoC** – Minimal reproducible code  
6. **Suggested Fix** – Remediation steps  
7. **Internal Discussion Record** – Per-persona analysis (R. Kent Dybvig, Rustacean, Geohot)

## Persona Discussion Guidelines

Simulate a short parallel discussion. Each persona contributes from their perspective:

- **R. Kent Dybvig**: GC design, memory layout, write barriers, incremental marking, allocator behavior.
- **Rustacean**: UB risks, unsafe usage, Send/Sync, lifetime/borrow soundness.
- **Geohot**: Exploit paths, race conditions, edge cases, “clever but brittle” mechanisms.

Then summarize the consensus or main conclusions.

---

## Verification Guidelines (避免誤判)

Based on [REPRODUCTION_REPORT.md](docs/issues/REPRODUCTION_REPORT.md). Before reporting an issue as **reproduced**, verify it against these patterns.

### Pattern 1: Full GC 會遮蔽 barrier 相關 bug

**現象**：Write barrier / generational barrier 失效的 issue，PoC 用 `collect_full()` 時測試通過。

**原因**：Full GC 從 roots 完整 trace，即使 barrier 沒記錄 OLD→YOUNG 引用，年輕物件仍可經由 trace 存活。

**建議**：測試 barrier 正確性時，必須用 **minor GC** (`collect()`)：
1. 先 `collect_full()` 將物件 promote 到 old gen
2. 再建立 OLD→YOUNG 引用
3. 呼叫 `collect()`（minor only）
4. 驗證 young 物件是否存活

### Pattern 2: 單執行緒無法觸發競態 bug

**現象**：TOCTOU、data race、concurrent access 類 issue，單執行緒 PoC 永遠不失敗。

**原因**：競態需多執行緒交錯執行；sequential test 無法可靠觸發。

**建議**：
- 在 issue 註記「需 Miri / ThreadSanitizer / 並發 PoC」
- 勿以單次執行通過就判定為誤判；可標記「未復現（可能需 TSan）」

### Pattern 3: 測試情境與 issue 描述不符

**現象**：Issue 描述跨執行緒 / orphan heap，但 PoC 為單執行緒。

**建議**：逐條對照 issue 的「預期觸發條件」與 PoC 是否一致。例如 bug1 需「執行緒終止 + 內部指標」，bug2 需「孤立 heap + Weak::upgrade」。

### Pattern 4: 容器內的 Gc 未被當作 root

**現象**：`Vec<Gc<T>>` 等容器存於 stack，但其 buffer 在 Rust heap，conservative scan 掃不到。

**建議**：PoC 若用 `Vec<Gc<T>>` 且在 GC 後存取，需使用 `register_test_root` 或 `register_test_root_region` 註冊 root，否則物件可能被誤收。

### Pattern 5: 難以觀察的內部狀態

**現象**：Bug 影響 barrier 是否 firing、flag 是否正確，但無直接 assert 可驗證。

**建議**：在 issue 註記「需 instrumentation 或 debug hook」；或設計可觀察的對外行為（例如年輕物件是否錯誤回收）作為 proxy。

### 誤判處理

若依上述檢視後認為是誤判：
1. **不要修復**，僅在 REPRODUCTION_REPORT 記錄為「疑似誤判」並簡述理由
2. **回報給使用者**，由使用者決定是否關閉 issue
