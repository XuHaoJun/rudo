# rudo-gc 增量收集器決策會議紀錄

> **會議日期**：2026-01-29
> **參與者**：R. Kent Dybvig（Chez Scheme 作者）、John McCarthy（Lisp 創始人）
> **主持人**：rudo-gc 开发团队
> **目的**：評估是否實作增量式 GC

---

## 1. 會議背景

### 1.1 問題陳述

rudo-gc 當前實作了：
- BiBOP 記憶體布局
- TLAB 分配
- Mark-Sweep GC
- 世代 GC 支援（GcCell dirty bit）
- 並行標記（Chase-Lev 工作竊取）
- 頁面擁有權追蹤

**核心問題**：是否需要實作增量式 GC（Incremental GC）以降低暫停時間？

### 1.2 現有架構概覽

```
rudo-gc 核心元件
├── heap.rs           # BiBOP, LocalHeap, TLAB
├── gc.rs             # Mark-Sweep 算法
├── marker.rs         # 並行標記、工作竊取
├── worklist.rs       # Chase-Lev Queue
├── cell.rs           # GcCell（世代 barrier）
├── scan.rs           # 保守堆疊掃描
└── ptr.rs            # Gc<T>, Weak<T>
```

### 1.3 v3 規格提案摘要

增量 GC v3 規格提出：
- 五階段狀態機（IDLE → INITIAL_MARK → INCREMENTAL_MARK → REMARK → SWEEPING）
- Dijkstra 寫入屏障
- 混合式清除（Lazy Sweep + Background Drop）
- GcCell 強制使用

---

## 2. 專家意見摘要

### 2.1 R. Kent Dybvig 的觀點

**背景**：Chez Scheme 作者，數十年 GC 實作經驗

#### 核心問題
> 「在決定是否做增量式 GC 之前，我需要問你幾個問題：
> 1. 你的目標用戶是誰？（REPL/交互式 vs 後端服務）
> 2. 你有遇到 STW 暫停的實際問題嗎？
> 3. GcCell 的 API 變更成本你計算過嗎？」

#### 建議：先不要做增量式 GC

理由：
1. **並行標記已經足夠**：rudo-gc 的 `ParallelMarkConfig` 讓使用者控制 worker 數量
2. **增量 GC 的真實成本**：
   - Dijkstra barrier 會拖慢所有寫入
   - REMARK 階段複雜
   - Lazy sweep 可能導致記憶體膨脹

#### 更簡單的替代方案
- 減少 `MARK_QUEUE_SIZE`，讓每個 chunk 更小
- 允許 Minor GC 更頻繁
- 增加平行 worker 數量

#### Chez Scheme 的歷史教訓
```
1980年代：嘗試增量 GC，發現 barrier 開銷太大
1990年代：專注於並行清除，放棄增量標記
現在的 Chez：仍然是 STW 標記 + 並行清除
```

#### 結論
> 「標記是 CPU-bound，盡量並行就好。清除是 I/O-bound，才需要增量處理。」

### 2.2 John McCarthy 的觀點

**背景**：Lisp 創始人，1959 年設計第一個 GC

#### 理論框架
增量 GC 需要 **三色標記** 和 **寫入屏障**，代價：

| 代價類型 | 說明 |
|----------|------|
| **空間** | 需要標記狀態 |
| **時間** | 每個寫入多一個檢查 |
| **正確性** | 需要 REMARK 階段確保完整性 |

#### 關鍵問題
> 「這些代價換來什麼？
> - 如果應用是 REPL，5ms 暫停很痛苦 → 值
> - 如果應用是後端服務，100ms 暫停可接受 → 不值」

#### 偏好：簡單優先
```
最簡單：什麼都不做（已並行了）
更簡單：只做 Lazy Sweep
簡單：優化現有程式碼
```

#### 實作建議：從清除開始
**Lazy Sweep 的優點**：
1. 清除是簡單的位元操作
2. 可延遲到下次分配時做
3. 不需要寫入屏障
4. 不需要 REMARK 階段

#### 最終建議
1. **先 profiling**：用數據說話
2. **先做 Lazy Sweep**：簡單、有效
3. **最後才考慮增量標記**：複雜、代價高

---

## 3. 技術分析

### 3.1 rudo-gc 現有並行標記能力

| 特性 | 狀態 | 說明 |
|------|------|------|
| Chase-Lev Queue | ✅ 已實作 | 無鎖工作竊取 |
| Push-based transfer | ✅ 已實作 | 減少竊取競爭 |
| 頁面擁有權追蹤 | ✅ 已實作 | 改善快取局部性 |
| GcCell dirty bit | ✅ 已實作 | 世代 GC barrier |
| Parallel Marking | ✅ 已實作 | 多執行緒標記 |

### 3.2 增量 GC 的邊際效益分析

```
GC 時間分佈（估計）
├── 根掃描：10-20%
├── 標記：30-40%
└── 清除：40-60%

增量標記的潛在收益：30-40% 的部分可中斷
但代價：每個寫入 +1 barrier 檢查
```

### 3.3 風險評估

| 風險 | 等級 | 說明 |
|------|------|------|
| GcCell API 變更 | 高 | 破壞性變更，用戶需重構 |
| Barrier 開銷 | 中 | 所有寫入多一次檢查 |
| REMARK 複雜度 | 中 | 需要額外 STW 階段 |
| Lazy Sweep 記憶體膨脹 | 低 | 可控 |

---

## 4. 決策

### 4.1 最終決定

**暫緩增量式 GC**，優先進行以下工作：

1. **Profiling 優先**：嚴格測量當前 GC 暫停時間分佈
2. **Lazy Sweep 實作**：作為低風險優化
3. **現有程式碼優化**：根據 profiling 結果針對性優化

### 4.2 待辦事項

| 優先順序 | 任務 | 說明 |
|----------|------|------|
| P0 | Profiling | 使用 perf/VTune 分析 GC 時間分佈 |
| P1 | Lazy Sweep | 實作延遲清除，將清除成本攤平到分配 |
| P2 | 現有優化 | 根據 profiling 結果優化現有程式碼 |
| P3 | 重新評估 | 根據數據決定是否繼續增量標記 |

### 4.3 成功標準

- [ ] 取得 GC 時間分佈數據（根掃描/標記/清除比例）
- [ ] Lazy Sweep 實作完成並通過測試
- [ ] 暫停時間符合目標（< 5ms @99.9%）

---

## 5. 附錄

### 5.1 參考資料

- Chez Scheme 源碼：`learn-projects/ChezScheme/c/gc.c`
- rudo-gc 設計分析：`docs/rudo-gc-design-analysis.md`
- 增量 GC v3 規格：`docs/incremental-gc-spec-3.md`

### 5.2 專家背景

| 專家 | 貢獻 | 相關經驗 |
|------|------|----------|
| R. Kent Dybvig | Chez Scheme 作者 | 數十年 GC 實作與優化 |
| John McCarthy | Lisp 發明者 | 1959 年設計第一個 GC |

### 5.3 術語對照表

| 英文 | 中文 | 說明 |
|------|------|------|
| Incremental GC | 增量 GC | 分多次執行的 GC |
| Mark-Sweep | 標記-清除 | 基本 GC 算法 |
| Tri-color Marking | 三色標記 | 增量 GC 的標記狀態 |
| Write Barrier | 寫入屏障 | 追蹤指標修改的機制 |
| Dijkstra Barrier | Dijkstra 屏障 | 一種寫入屏障算法 |
| Lazy Sweep | 延遲清除 | 清除延遲到分配時進行 |
| STW (Stop-The-World) | 停頓世界 | 所有執行緒暫停 |
| TLAB | 執行緒本地分配緩衝 | 無鎖分配優化 |

---

> **會議紀錄結束**
> 日期：2026-01-29
> 文件版本：v1.0
