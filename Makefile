# Makefile for Mesh-Talk

# Development commands
.PHONY: dev build test clean check

dev: install-deps frontend-install
	@echo "Development environment configured!"
	./scripts/setup-hooks.sh

build:
	cd src-tauri && cargo build --release

test:
	cd src-tauri && cargo test

clean:
	cd src-tauri && cargo clean

# Frontend commands
.PHONY: frontend-dev frontend-build

frontend-dev:
	cd frontend && npm run dev

frontend-build:
	cd frontend && npm run build

# Install dependencies
.PHONY: install-deps frontend-install check lint fix format

install-deps:
	cd src-tauri && cargo build

frontend-install:
	cd frontend && npm install

check:
	chmod +x scripts/check-health.sh
	./scripts/check-health.sh

lint:
	cd src-tauri && cargo clippy -- -D warnings

fix:
	cd src-tauri && cargo clippy --fix --allow-dirty --allow-staged
	cd src-tauri && cargo fmt
	cd frontend && npx prettier --write src/

format:
	cd src-tauri && cargo fmt
	cd frontend && npx prettier --write src/

# Tauri commands
.PHONY: tauri-dev tauri-build

tauri-dev:
	cd src-tauri && cargo tauri dev

tauri-build:
	cd src-tauri && cargo tauri build

# Help
.PHONY: help

help:
	@echo "Mesh-Talk Makefile"
	@echo ""
	@echo "Usage:"
	@echo "  make dev              Set up the development environment"
	@echo "  make build            Build the application for release"
	@echo "  make test             Run tests"
	@echo "  make clean            Clean build artifacts"
	@echo "  make check            Run all quality checks"
	@echo "  make lint             Run linting tools"
	@echo "  make fix              Automatically fix code issues"
	@echo "  make format           Format code"
	@echo "  make frontend-dev     Run the frontend in development mode"
	@echo "  make frontend-build   Build the frontend for production"
	@echo "  make install-deps     Install Rust dependencies"
	@echo "  make frontend-install Install frontend dependencies"
	@echo "  make tauri-dev        Run Tauri app in development mode"
	@echo "  make tauri-build      Build Tauri app for distribution"