# Makefile for Mesh-Talk

# Development commands
.PHONY: dev build test test-focused test-plan test-changed verify-plan verify-changed validation-report validation-audit test-timing e2e eval-smoke ai-eval smoke-full clean clean-builds clean-builds-dry-run check

# Managed commands coordinate with cache eviction across all local worktrees.
MANAGED_BUILD = python3 scripts/build-retention.py run --

dev: install-deps frontend-install
	@echo "Development environment configured!"
	./scripts/setup-hooks.sh
	./scripts/install-auto-clean-macos.sh

build:
	python3 scripts/build-retention.py run --archive binary -- cargo build --release

test:
	$(MANAGED_BUILD) node scripts/validation/stage.mjs workspace-tests --profile shared --resource heavy -- cargo test --workspace

# Normal development: select the affected crate and test filter, or one integration
# target. Keep `make test` as the explicit comprehensive pre-PR/CI command.
test-focused:
	@test -n "$(TEST_PACKAGE)" && { test -n "$(TEST_FILTER)" || test -n "$(TEST_TARGET)"; } || { echo "Usage: make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync [TEST_TARGET=bridge_cross_lan]" >&2; exit 2; }
	@if [ -n "$(TEST_TARGET)" ]; then \
		$(MANAGED_BUILD) node scripts/validation/stage.mjs rust-focused --profile shared --resource heavy -- cargo test -p "$(TEST_PACKAGE)" --test "$(TEST_TARGET)" "$(TEST_FILTER)"; \
	else \
		$(MANAGED_BUILD) node scripts/validation/stage.mjs rust-focused --profile shared --resource heavy -- cargo test -p "$(TEST_PACKAGE)" --lib "$(TEST_FILTER)"; \
	fi

# Development-only affected tests. Preview first; pass literal planner options with
# VALIDATION_ARGS (for example --base origin/main). Required PR gates are unchanged.
test-plan:
	node scripts/validation/changed.mjs $(VALIDATION_ARGS)

test-changed:
	$(MANAGED_BUILD) node scripts/validation/changed.mjs --run $(VALIDATION_ARGS)

verify-plan:
	node scripts/validation/changed.mjs --checks $(VALIDATION_ARGS)

verify-changed:
	node scripts/validation/changed.mjs --checks --run --managed $(VALIDATION_ARGS)

validation-report:
	@node scripts/validation/latency.mjs $(VALIDATION_ARGS)

validation-audit:
	@node scripts/validation/selection-audit.mjs $(VALIDATION_ARGS)

test-timing:
	@node scripts/validation/libtest-timing.mjs $(VALIDATION_ARGS)

# End-to-end integration tests: real `mesh-talk-node` processes over UDP discovery +
# TCP (two-node DM, history-across-restart, post-office offline delivery,
# and opt-in cross-LAN bridge discovery). `#[ignore]`d
# by default (real UDP-multicast discovery + node cold starts), so run them explicitly here. Serial
# (--test-threads=1) to avoid discovery-port / CPU contention between the heavy processes.
e2e:
	$(MANAGED_BUILD) nice -n 10 cargo test -p mesh-talk-core --test two_node_cli --test persistent_history --test post_office_offline --test channel_and_file_cli --test bridge_cross_lan -- --ignored --test-threads=1
	$(MANAGED_BUILD) nice -n 10 cargo test -p mesh-talk-core --lib large_directory_enqueue_uses_compact_manifest_instead_of_frame_error -- --ignored --test-threads=1

eval-smoke:
	./scripts/ai-eval-smoke.sh

ai-eval:
	node scripts/ai-eval.mjs

smoke-full:
	./scripts/run-real-smoke.sh

clean:
	$(MANAGED_BUILD) cargo clean

clean-builds:
	python3 scripts/build-retention.py clean

clean-builds-dry-run:
	python3 scripts/build-retention.py clean --dry-run

# Frontend commands
.PHONY: frontend-dev frontend-build

frontend-dev:
	cd frontend && npm run dev

frontend-build:
	cd frontend && npm run build

# Install dependencies
.PHONY: install-deps frontend-install check lint fix format

install-deps:
	$(MANAGED_BUILD) cargo build

frontend-install:
	cd frontend && npm install

check:
	chmod +x scripts/check-health.sh
	$(MANAGED_BUILD) ./scripts/check-health.sh

lint:
	$(MANAGED_BUILD) cargo clippy --workspace --all-targets -- -D warnings

fix:
	cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged
	cargo fmt --all
	cd frontend && npm run format

format:
	cargo fmt --all
	cd frontend && npm run format

# Tauri commands
.PHONY: tauri-dev tauri-build

tauri-dev:
	$(MANAGED_BUILD) cargo tauri dev

tauri-build:
	python3 scripts/build-retention.py run --archive bundle -- node frontend/node_modules/@tauri-apps/cli/tauri.js build

# Help
.PHONY: help

help:
	@echo "Mesh-Talk Makefile"
	@echo ""
	@echo "Usage:"
	@echo "  make dev              Set up the development environment"
	@echo "  make build            Build the application for release"
	@echo "  make test             Run the workspace unit/integration tests"
	@echo "  make test-focused TEST_PACKAGE=... TEST_FILTER=...  Run affected Rust tests"
	@echo "  make test-plan        Preview affected development tests and selection reasons"
	@echo "  make test-changed     Run the affected plan; block unknown/no-test selections"
	@echo "  make verify-plan      Preview affected tests and static obligations"
	@echo "  make verify-changed   Run affected formatting, lint/types, builds and tests"
	@echo "  make validation-report Report local validation latency; pass --repo/--input via VALIDATION_ARGS"
	@echo "  make validation-audit Compare an affected plan with broader failure evidence"
	@echo "  make test-timing      Estimate serial libtest gaps; pass --input and --serial via VALIDATION_ARGS"
	@echo "  make e2e              Run the end-to-end multi-process tests (slow)"
	@echo "  make eval-smoke       Run delivery and AI regression smoke checks"
	@echo "  make ai-eval          Run real model-backed AI evals (requires AI_EVAL_COMMAND)"
	@echo "  make smoke-full       Run full real smoke: Rust/frontend unit + backend/UI E2E"
	@echo "  make clean            Clean build artifacts"
	@echo "  make clean-builds     Apply five-build retention and 40 GiB cache budget"
	@echo "  make clean-builds-dry-run Preview build retention across worktrees"
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
