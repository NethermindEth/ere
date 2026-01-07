#!/bin/bash
set -e

# --- Utility functions (duplicated) ---
# Checks if a tool is installed and available in PATH.
is_tool_installed() {
    command -v "$1" &> /dev/null
}

# Ensures a tool is installed. Exits with an error if not.
ensure_tool_installed() {
    local tool_name="$1"
    local purpose_message="$2"
    if ! is_tool_installed "${tool_name}"; then
        echo "Error: Required tool '${tool_name}' could not be found." >&2
        if [ -n "${purpose_message}" ]; then
            echo "       It is needed ${purpose_message}." >&2
        fi
        echo "       Please install it first and ensure it is in your PATH." >&2
        exit 1
    fi
}
# --- End of Utility functions ---

echo "Installing ZKM Toolchain using zkmup (latest release versions)..."

# Prerequisites for zkmup
ensure_tool_installed "curl" "to download the zkmup installer"
ensure_tool_installed "sh" "as the zkmup installer script uses sh"

ZIREM_VERSION="1.2.3"

# Step 1: Download and run the script that installs the zkmup binary itself.
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/ProjectZKM/toolchain/refs/heads/main/setup.sh | sh

# Step 2: Source the environment file to ensure zkmup is available
if [ -f "${HOME}/.zkm-toolchain/env" ]; then
    . "${HOME}/.zkm-toolchain/env"
fi

# Step 3: Ensure the installed zkmup script is in PATH (fallback if sourcing didn't work)
export PATH="${PATH}:${HOME}/.zkm-toolchain/bin"

# Step 4: Link the latest toolchain as toolchain `zkm`
# Get the latest available version from zkmup
LATEST_VERSION=$(zkmup list-available 2>/dev/null | head -1 | cut -d' ' -f1)
if [ -z "${LATEST_VERSION}" ]; then
    echo "Error: Failed to get available ZKM toolchain versions from zkmup" >&2
    exit 1
fi

# Find the toolchain directory
TOOLCHAIN_PATH="${HOME}/.zkm-toolchain/${LATEST_VERSION}"
if [ ! -d "${TOOLCHAIN_PATH}" ]; then
    # Try to find any installed toolchain
    TOOLCHAIN_PATH=$(ls -d ${HOME}/.zkm-toolchain/*/ 2>/dev/null | grep -v bin | grep -v env | head -1)
fi

if [ -z "${TOOLCHAIN_PATH}" ] || [ ! -d "${TOOLCHAIN_PATH}" ]; then
    echo "Error: Could not find ZKM toolchain directory" >&2
    echo "Available directories in ~/.zkm-toolchain:" >&2
    ls -la "${HOME}/.zkm-toolchain/" >&2 || true
    exit 1
fi

echo "Linking ZKM toolchain from: ${TOOLCHAIN_PATH}"
rustup toolchain link zkm "${TOOLCHAIN_PATH}"

# Step 5: Install cargo-ziren by building from source
# Note: The Dockerfile sets nightly as default, so we don't need +nightly here.
# Using +nightly can fail if cargo doesn't recognize the toolchain selector syntax.
cargo install --locked --git https://github.com/ProjectZKM/Ziren.git --tag "v${ZIREM_VERSION}" zkm-cli

# Verify ZKM installation
echo "Verifying ZKM installation..."

echo "Checking for 'zkm' toolchain..."
if rustup +zkm toolchain list | grep -q "zkm"; then
    echo "ZKM Rust toolchain found."
else
    echo "Error: ZKM Rust toolchain ('zkm') not found after installation!" >&2
    exit 1
fi

echo "Checking for cargo-ziren CLI tool..."
if cargo ziren --version; then
    echo "cargo-ziren CLI tool verified successfully."
else
    echo "Error: 'cargo ziren --version' failed." >&2
    exit 1
fi