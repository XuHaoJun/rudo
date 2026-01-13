#!/bin/bash

# Run tests sequentially to avoid GC interference between parallel test threads.
# When tests run in parallel, one test's GC can interfere with another test's
# heap-allocated data structures (like Vec<Gc<T>>) that aren't directly traced
# from stack roots. This is a fundamental limitation of conservative GC.
cargo test --workspace --lib --bins --tests --all-features -- --include-ignored --test-threads=1