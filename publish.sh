#!/bin/bash
set -e  # Exit on error

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to get crate name from Cargo.toml
get_crate_name() {
    local crate_path="$1"
    grep -E '^name\s*=' "$crate_path/Cargo.toml" | head -1 | sed -E 's/^name\s*=\s*"([^"]+)".*/\1/'
}

# Function to get version from Cargo.toml (workspace or local)
get_crate_version() {
    local crate_path="$1"
    local version_line=$(grep -E '^version\s*=' "$crate_path/Cargo.toml" | head -1)
    
    if echo "$version_line" | grep -q 'workspace\s*=\s*true'; then
        # Get version from workspace Cargo.toml
        grep -E '^version\s*=' "Cargo.toml" | head -1 | sed -E 's/^version\s*=\s*"([^"]+)".*/\1/'
    else
        # Get version from crate Cargo.toml
        echo "$version_line" | sed -E 's/^version\s*=\s*"([^"]+)".*/\1/'
    fi
}

# Function to check if version exists on crates.io
version_exists() {
    local crate_name="$1"
    local version="$2"
    
    # Use crates.io API to check if version exists
    local response=$(curl -s "https://crates.io/api/v1/crates/${crate_name}/${version}" 2>/dev/null)
    
    if echo "$response" | grep -q '"version"'; then
        return 0  # Version exists
    else
        return 1  # Version does not exist
    fi
}

# Crate publish order (dependencies first)
CRATES=(
    "crates/sys_alloc"
    "crates/rudo-gc-tokio-derive"
    "crates/rudo-gc-derive"
    "crates/rudo-gc"
)

echo -e "${YELLOW}Starting cargo publish for all crates...${NC}"
echo ""

for crate in "${CRATES[@]}"; do
    crate_name=$(get_crate_name "$crate")
    crate_version=$(get_crate_version "$crate")
    
    echo -e "${BLUE}Checking ${crate_name} v${crate_version}...${NC}"
    
    if version_exists "$crate_name" "$crate_version"; then
        echo -e "${YELLOW}⚠ Version ${crate_name} v${crate_version} already exists on crates.io${NC}"
        echo -e "${YELLOW}Skipping ${crate}...${NC}"
        echo ""
        continue
    fi
    
    echo -e "${GREEN}Publishing ${crate_name} v${crate_version}...${NC}"
    cd "$crate"
    cargo publish
    cd - > /dev/null
    echo -e "${GREEN}✓ Successfully published ${crate_name} v${crate_version}${NC}"
    echo ""
done

echo -e "${GREEN}All crates processed successfully!${NC}"
