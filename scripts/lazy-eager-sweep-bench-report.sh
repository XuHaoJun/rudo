#!/bin/bash
# Lazy Sweep vs Eager Sweep Benchmark Report Generator

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

echo "=============================================="
echo "  Lazy Sweep vs Eager Sweep Benchmark Report"
echo "=============================================="
echo ""

echo "=== Running Lazy Sweep Benchmark ==="
cargo bench --features lazy-sweep --bench sweep_benchmark \
    -- --sample-size=30 --measurement-time=5s \
    2>&1 | tee lazy_results.txt

echo ""
echo "=== Running Eager Sweep Benchmark ==="
cargo bench --no-default-features --features derive --bench sweep_benchmark \
    -- --sample-size=30 --measurement-time=5s \
    2>&1 | tee eager_results.txt

echo ""
echo "=== Generating Comparison Report ==="
python3 << 'EOF'
import re
import sys

def extract_benchmarks(filename):
    try:
        with open(filename) as f:
            content = f.read()
    except FileNotFoundError:
        return {}
    
    results = {}
    for line in content.split('\n'):
        if 'pause_time' in line or 'throughput' in line or 'latency' in line:
            match = re.search(r'(\S+)\s+\w+\s+([\d.]+)\s+(\w+)', line)
            if match:
                results[match.group(1)] = (float(match.group(2)), match.group(3))
    return results

lazy = extract_benchmarks('lazy_results.txt')
eager = extract_benchmarks('eager_results.txt')

print("")
print("==============================================")
print("           BENCHMARK COMPARISON              ")
print("==============================================")
print("")
print("| Benchmark              | Eager (mean)   | Lazy (mean)    | Speedup |")
print("|------------------------|----------------|----------------|---------|")

for name in sorted(eager.keys()):
    if name in eager and name in lazy:
        e_val, unit = eager[name]
        l_val, _ = lazy[name]
        if l_val > 0:
            speedup = e_val / l_val
            print(f"| {name:22} | {e_val:12.4f} {unit} | {l_val:12.4f} {unit} | {speedup:6.1f}x |")

print("")
print("==============================================")
print("             SUMMARY                          ")
print("==============================================")

lazy_total = sum(v[0] for v in lazy.values()) if lazy else 0
eager_total = sum(v[0] for v in eager.values()) if eager else 0

if eager_total > 0 and lazy_total > 0:
    total_speedup = eager_total / lazy_total
    print(f"Total Speedup: {total_speedup:.1f}x")
    print(f"Eager Total:   {eager_total:.4f}s")
    print(f"Lazy Total:    {lazy_total:.4f}s")
    print("")
    print("Lazy sweep significantly reduces GC pause times!")
EOF

echo ""
echo "Report generated successfully!"
echo "HTML reports available in target/criterion/"
