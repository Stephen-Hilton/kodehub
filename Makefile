# Kodehub pilot. Everything (data, cache, renders) lives under ./.kode.
SHELL := /bin/bash
export KODEHUB_HOME ?= $(CURDIR)/.kode
export KODEHUB_ARANGO_URL ?= http://127.0.0.1:8528
export KODEHUB_ARANGO_PASSWORD ?= kodehub
export KODEHUB_DB ?= kodehub
# Optional: a file of env vars (e.g. DEV#_TOKEN for headless agents). Only its *path* is passed to
# the server; scripts/claude-as.sh sources it in its own short-lived process, so the server never
# holds a token. Set KODEHUB_ENV_FILE in a git-ignored local.mk.
-include local.mk
export KODEHUB_ENV_FILE ?=

.PHONY: help db db-down build dev ui-build desktop desktop-build seed check fmt lint test test-unit test-integration app-check ui-test live-test clean

help:
	@echo "make dev      start ArangoDB + kodehub-server"
	@echo "make seed     import examples/auth-poc (server must be running)"
	@echo "make desktop  Tauri desktop app (with make dev running)"
	@echo "make live-test  live agent tests on the Claude Code backend (DEV#_TOKEN by name)"
	@echo "make check    fmt, clippy -D warnings, unit + integration tests, UI typecheck/unit + Playwright e2e"
	@echo "make ui-test  Playwright end-to-end tests of the desktop UI (web build)"

db:
	@docker compose up -d arangodb >/dev/null
	@for i in $$(seq 1 60); do curl -sf -u root:$(KODEHUB_ARANGO_PASSWORD) $(KODEHUB_ARANGO_URL)/_api/version >/dev/null && exit 0; sleep 1; done; echo "ArangoDB did not start"; exit 1

db-down:
	docker compose down

build:
	cargo build --bins

dev: db build ui-build
	@mkdir -p $(KODEHUB_HOME)
	@echo "Kodehub UI: http://127.0.0.1:7878  (desktop app: make desktop)"
	./target/debug/kodehub-server --static-dir app/dist

ui-build:
	@if [ -d app/node_modules ]; then cd app && npm run --silent build >/dev/null; fi

# Tauri desktop shell (expects `make dev` running in another terminal)
desktop:
	cd app && npx tauri dev

# Build the desktop app bundle (app/src-tauri/target/release/bundle/macos/Kodehub.app)
desktop-build:
	cd app && npx tauri build --bundles app

seed: build
	./target/debug/kodehub seed examples/auth-poc

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test-unit:
	cargo test --workspace --lib --bins

test-integration: db build
	cargo test --workspace --test '*'

test: test-unit test-integration

app-check:
	@if [ -d app/node_modules ]; then cd app && npm run --silent typecheck && npm run --silent test:unit; else echo "app/node_modules missing: run 'cd app && npm install'"; exit 1; fi

check: fmt lint test app-check ui-test
	@echo "make check: all green"

# Live tests with real Claude Code agents (subscription token chosen by env var NAME).
live-test: db build
	KODEHUB_LIVE_TOKEN_ENV=$${KODEHUB_LIVE_TOKEN_ENV:-DEV1_TOKEN} KODEHUB_LIVE_DEMO=1 cargo test -p kodehub-server --test m4_claude_mcp --test m5_live_claude_code -- --nocapture

ui-test: db build
	cd app && npx playwright test

clean:
	cargo clean
