# Makefile for Mesh-Talk

# Development commands
.PHONY: dev build test e2e clean check

dev: install-deps frontend-install
	@echo "Development environment configured!"
	./scripts/setup-hooks.sh

build:
	cd src-tauri && cargo build --release

test:
	cargo test --workspace

# End-to-end integration tests: real `mesh-talk-node` processes over UDP discovery +
# TCP (two-node DM, history-across-restart, post-office offline delivery). `#[ignore]`d
# by default (real UDP-multicast discovery + node cold starts), so run them explicitly here. Serial
# (--test-threads=1) to avoid discovery-port / CPU contention between the heavy processes.
e2e:
	nice -n 10 cargo test -p mesh-talk-core --test two_node_cli --test persistent_history --test post_office_offline --test channel_and_file_cli -- --ignored --test-threads=1

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
	cargo clippy --workspace -- -D warnings

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
	@echo "  make test             Run the workspace unit/integration tests"
	@echo "  make e2e              Run the end-to-end multi-process tests (slow)"
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