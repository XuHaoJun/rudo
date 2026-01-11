# 專案進度報告 2：rudo-gc 多執行緒 BiBOP 與保守式掃描實作總結

**致 John McCarthy 博士：**

我們非常榮幸地向您報告，`rudo-gc` 已從單執行緒實驗階段邁向了成熟的多執行緒並行架構。依照您的「方案 A」與「BiBOP 幾何學」指導方針，我們已成功建立了足以應對複雜 Rust 環境的保守式垃圾回收系統。以下是詳細的進度報告：

## 1. 並行協調與 Safepoint 握手機制
我們已實作了完整的 **Stop-the-World (STW)** 握手協定：
- **協作式 Safepoint**：透過 `AtomicBool` 全域標記與 `Tlab::alloc` 中的隱式檢查，確保所有執行緒能快速且安全地進入同步狀態。
- **執行緒註冊機制**：引入了 `ThreadControlBlock` 與全域 `ThreadRegistry`，能精確追蹤所有活躍執行緒的狀態（EXECUTING, SAFEPOINT, INACTIVE）。
- **同步正確性**：特別解決了 GC 期間新執行緒產生的競態條件，透過全域 `gc_in_progress` 標記防止新線程誤入握手死鎖。

## 2. 強化版保守式掃描 (Conservative Scanning)
針對您最關注的「尋根」問題，我們已實作了具備內部指針支援的掃描器：
- **暫存器溢出 (Register Spilling)**：使用 x86_64 組合語言精確捕捉 `RBX`, `RBP`, `R12-R15` 等被調用者保存暫存器。
- **內部指針 (Interior Pointers)**：利用 BiBOP 的頁面對齊特性，在 O(1) 時間內從任何內部指針反推 Page Header。針對大物件空間 (LOS)，我們建立了 `large_object_map` 以支援跨頁面的內部指針識別。
- **精確過濾**：透過魔數 (Magic Number) 與頁面存活檢查，極大地降低了「偽陽性 (False Positive)」對回收效率的影響。

## 3. 分層式 BiBOP 分配器：TLAB 與 GlobalHeap
記憶體分配效能已達到工業級水準：
- **TLAB (Thread-Local Allocation Buffer)**：每個執行緒擁有獨立的 8 組 Size Classes 緩衝區，採用 Bump-pointer 分配，完全避開了分配熱點的鎖競爭。
- **插槽重用 (Slot Reuse)**：當 TLAB 耗盡時，系統會先嘗試從現有 Page 的 `free_list` 中回收已標記的空閒插槽，最大限度地提高記憶體緊湊度。
- **大物件空間 (Large Object Space)**：支援超過 2KB 的物件分配，並在 Page Header 中設置獨立 Flag 進行特化管理。

## 4. 數學防線的具體化：著色與黑名單
我們已建立了初步的數學防線以防禦記憶體洩漏：
- **地址空間著色 (Coloring)**：強迫堆記憶體落點於 `0x6000_0000_0000` 附近，這不僅有利於過濾非指針整數，也為未來的「指針標籤 (Pointer Tagging)」留下了實驗空間。
- **預分配黑名單 (Blacklisting)**：在 `GlobalSegmentManager` 分配新頁面前，會主動掃描當前堆疊與暫存器，若發現與新頁面地址衝突的「定時炸彈」，將立即進行隔離 (Quarantine)。

## 5. 世代回收與寫入屏障 (Generational GC & Write Barrier)
雖然目前仍是 Non-moving 架構，但我們已實作了世代區隔：
- **世代管理**：Page Header 包含 `generation` 標記，區分年輕代 (0) 與老年代 (1)。
- **寫入屏障**：透過 `GcCell` 的 `deref_mut` 觸發 `is_dirty` 標記，讓 Minor GC 能快速掃描老年代對年輕代的引用。

---

## 下一步行動規劃

雖然我們已完成了堅實的基礎，但為了追求極致，我們建議下一階段的開發重點：
1. **並行標記 (Parallel Marking)**：目前標記仍由單一 Collector 執行緒完成，我們計畫引入「Message Passing」(Chez Scheme (Remote Mentions) 隊列以加速大型堆的掃描。
2. **手動 Safepoint 注入**：在 Rust 的長迴圈中自動或手動注入更多的 `safepoint()` 呼叫，以降低 GC 停頓的延遲。
3. **Miri 驗證與正式化**：持續加強在 Miri 下的測試，確保無任何未定義行為 (UB)。

**您的學生與追隨者，**
*rudo-gc 開發團隊*
