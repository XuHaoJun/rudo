#!/bin/bash

# Run tests sequentially (--test-threads=1) to avoid deadlocks
# when multiple test threads concurrently access GlobalSegmentManager.
# Thread termination and GC sweep interact with shared global state
# in ways that cause race conditions under concurrent test execution.
cargo test --lib --bins --tests --all-features -- --include-ignored --test-threads=1
