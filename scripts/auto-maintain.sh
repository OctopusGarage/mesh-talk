#!/bin/bash

# Automatic code maintenance script for Mesh-Talk project
# This script runs regular maintenance tasks to keep the codebase clean and up-to-date

set -e  # Exit on any error

echo "Running Mesh-Talk automatic code maintenance..."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    case $1 in
        "success")
            echo -e "${GREEN}✓${NC} $2"
            ;;
        "warning")
            echo -e "${YELLOW}⚠${NC} $2"
            ;;
        "error")
            echo -e "${RED}✗${NC} $2"
            ;;
        *)
            echo "$2"
            ;;
    esac
}

# Check if we're in the project root
if [ ! -f "Makefile" ] || [ ! -d "src-tauri" ] || [ ! -d "frontend" ]; then
    print_status "error" "Please run this script from the project root directory"
    exit 1
fi

# Check Rust toolchain
print_status "success" "Checking Rust toolchain..."
if ! command -v cargo >/dev/null 2>&1; then
    print_status "error" "Cargo not found. Please install Rust: https://www.rust-lang.org/"
    exit 1
fi

# Check Node.js and npm
print_status "success" "Checking Node.js and npm..."
if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    print_status "error" "Node.js or npm not found. Please install Node.js: https://nodejs.org/"
    exit 1
fi

# Run automatic code formatting
print_status "success" "Running automatic code formatting..."
if ! make format; then
    print_status "error" "Failed to run automatic code formatting"
    exit 1
fi

# Run automatic code fixes
print_status "success" "Running automatic code fixes..."
if ! make fix; then
    print_status "warning" "Some automatic code fixes failed"
fi

# Update dependencies
print_status "success" "Updating dependencies..."
if ! make install-deps; then
    print_status "warning" "Failed to update some dependencies"
fi

if ! make frontend-install; then
    print_status "warning" "Failed to update some frontend dependencies"
fi

# Run code quality checks
print_status "success" "Running code quality checks..."
if ! make check; then
    print_status "error" "Code quality checks failed"
    exit 1
fi

# Run tests
print_status "success" "Running tests..."
if ! cd src-tauri && cargo test; then
    print_status "error" "Tests failed"
    exit 1
fi
cd ..

# Check for security vulnerabilities
print_status "success" "Checking for security vulnerabilities..."
if command -v cargo-audit >/dev/null 2>&1; then
    if ! cd src-tauri && cargo audit; then
        print_status "warning" "Security vulnerabilities found in Rust dependencies"
    fi
    cd ..
else
    print_status "warning" "cargo-audit not found. Skipping security audit."
fi

# Build the project
print_status "success" "Building the project..."
if ! make build; then
    print_status "error" "Build failed"
    exit 1
fi

print_status "success" "Automatic code maintenance completed successfully!"
exit 0