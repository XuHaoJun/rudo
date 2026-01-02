#!/bin/bash

MIRIFLAGS="-Zmiri-ignore-leaks" cargo +nightly miri test --features test-util