#!/bin/bash
# GUI Overall Benchmark Runner
# 
# Runs GUI benchmark scenarios and reports timing
# Uses RUST_TEST_THREADS=1 to avoid GC issues

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="$SCRIPT_DIR/bench_output/gui_overall"

mkdir -p "$OUTPUT_DIR"

echo "========================================"
echo "GUI Overall Benchmark Runner"
echo "========================================"
echo ""

cd "$SCRIPT_DIR"

# Build benchmark
echo "Building benchmark..."
cargo build --release --bench gui_overall 2>&1 | tail -3
echo ""

# List of benchmark scenarios
SCENARIOS=(
    "tree_build_1k"
    "tree_build_10k"
    "reactive_update_1k"
    "reactive_update_10k"
    "partial_destroy"
    "full_cycle"
    "sustained_60fps"
)

TOTAL=${#SCENARIOS[@]}
CURRENT=0

echo "Running benchmarks..."
echo ""

for SCENARIO in "${SCENARIOS[@]}"; do
    CURRENT=$((CURRENT + 1))
    echo "[$CURRENT/$TOTAL] $SCENARIO"
    
    # Run and measure time
    START=$(date +%s.%N)
    
    RUST_TEST_THREADS=1 timeout 300 cargo test --bench gui_overall --no-run 2>&1 | tail -1
    RUST_TEST_THREADS=1 timeout 300 target/release/deps/gui_overall-* --test "$SCENARIO" 2>&1 | tail -3
    
    END=$(date +%s.%N)
    ELAPSED=$(echo "$END - $START" | bc)
    
    echo "    Time: ${ELAPSED}s"
    echo ""
done

echo "========================================"
echo "Done!"
echo "========================================"
