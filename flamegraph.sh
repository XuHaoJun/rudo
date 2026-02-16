#!/bin/bash
# GUI Overall Flamegraph Generator
# 
# Generates flamegraphs for GUI benchmark using cargo-flamegraph

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="$SCRIPT_DIR/bench_output/gui_overall"

mkdir -p "$OUTPUT_DIR"

echo "========================================"
echo "GUI Overall Flamegraph Generator"
echo "========================================"
echo ""
echo "Output directory: $OUTPUT_DIR"
echo ""

cd "$SCRIPT_DIR"

# Build example with release for better performance
echo "Building example..."
cargo build --example gui_benchmark --release 2>&1 | tail -3
echo ""

echo "Generating flamegraph..."
echo ""

# Run cargo flamegraph
# Using RUST_TEST_THREADS=1 to avoid GC issues between threads
# Using release build for realistic performance
RUST_TEST_THREADS=1 cargo flamegraph \
    --example gui_benchmark \
    -o "$OUTPUT_DIR/gui_benchmark.svg" \
    2>&1 | tail -15

if [ -f "$OUTPUT_DIR/gui_benchmark.svg" ]; then
    SIZE=$(ls -lh "$OUTPUT_DIR/gui_benchmark.svg" | awk '{print $5}')
    echo ""
    echo "✓ Generated gui_benchmark.svg ($SIZE)"
else
    echo ""
    echo "✗ Failed to generate flamegraph"
fi

echo ""
echo "========================================"
echo "Done!"
echo "========================================"
echo ""
echo "Output files:"
ls -la "$OUTPUT_DIR"
echo ""
echo "Open in browser:"
echo "  firefox $OUTPUT_DIR/gui_benchmark.svg"
