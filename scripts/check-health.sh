#!/bin/bash

# Unified health check script for Mesh-Talk project
# This script runs all code quality checks and validations

set -e  # Exit on any error

echo "Running Mesh-Talk health checks..."

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

# Function to check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Check if we're in the project root
if [ ! -f "Makefile" ] || [ ! -d "src-tauri" ] || [ ! -d "frontend" ]; then
    print_status "error" "Please run this script from the project root directory"
    exit 1
fi

# Check Rust toolchain
print_status "success" "Checking Rust toolchain..."
if ! command_exists cargo; then
    print_status "error" "Cargo not found. Please install Rust: https://www.rust-lang.org/"
    exit 1
fi

# Check Node.js and npm
print_status "success" "Checking Node.js and npm..."
if ! command_exists node || ! command_exists npm; then
    print_status "error" "Node.js or npm not found. Please install Node.js: https://nodejs.org/"
    exit 1
fi

# Install dependencies if not already installed
print_status "success" "Installing dependencies..."
make install-deps >/dev/null 2>&1 || true
make frontend-install >/dev/null 2>&1 || true

# Check code formatting (Rust)
print_status "success" "Checking Rust code formatting..."
if ! cd src-tauri && cargo fmt -- --check; then
    print_status "error" "Rust code formatting issues found. Run 'cargo fmt' to fix."
    exit 1
fi
cd ..

# Check code formatting (Frontend)
print_status "success" "Checking frontend code formatting..."
if ! cd frontend && npx prettier --check src/; then
    print_status "error" "Frontend code formatting issues found. Run 'npx prettier --write src/' to fix."
    exit 1
fi
cd ..

# Run Clippy (Rust linter)
print_status "success" "Running Rust linter (Clippy)..."
if ! cd src-tauri && cargo clippy -- -D warnings; then
    print_status "error" "Rust linting issues found. Please fix Clippy warnings."
    exit 1
fi
cd ..

# Run ESLint (Frontend linter)
print_status "success" "Running frontend linter (ESLint)..."
if ! cd frontend && npx eslint src/ --ext .js,.ts,.vue; then
    print_status "error" "Frontend linting issues found. Please fix ESLint errors."
    exit 1
fi
cd ..

# Run tests
print_status "success" "Running tests..."
if ! cd src-tauri && cargo test; then
    print_status "error" "Tests failed. Please fix test issues."
    exit 1
fi
cd ..

# Check for security vulnerabilities (Rust)
print_status "success" "Checking for Rust security vulnerabilities..."
if ! command_exists cargo-audit; then
    print_status "warning" "cargo-audit not found. Installing..."
    if cargo install cargo-audit; then
        print_status "success" "cargo-audit installed successfully"
        if ! cd src-tauri && cargo audit; then
            print_status "warning" "Security vulnerabilities found in Rust dependencies."
        fi
        cd ..
    else
        print_status "error" "Failed to install cargo-audit"
        exit 1
    fi
else
    if ! cd src-tauri && cargo audit; then
        print_status "warning" "Security vulnerabilities found in Rust dependencies."
    fi
    cd ..
fi

# Check for security vulnerabilities (Node.js)
print_status "success" "Checking for Node.js security vulnerabilities..."
if ! command_exists npx; then
    print_status "error" "npx not found. Please install Node.js: https://nodejs.org/"
    exit 1
fi

# Ensure audit-ci is installed
if ! cd frontend && npx audit-ci --config audit-ci.json; then
    print_status "warning" "Security vulnerabilities found in Node.js dependencies or audit-ci.json not found."
fi
cd .. || true

# Build the project
print_status "success" "Building the project..."
if ! cd src-tauri && cargo build --verbose; then
    print_status "error" "Build failed. Please fix build issues."
    exit 1
fi
cd ..

# Build the frontend
print_status "success" "Building the frontend..."
if ! cd frontend && npm run build; then
    print_status "error" "Frontend build failed. Please fix build issues."
    exit 1
fi
cd ..

print_status "success" "All health checks passed!"
exit 0