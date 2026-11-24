# Kodehub v0.1. Everything runs on Kubernetes (Kind on a laptop, spec §3); local state (build
# cache, renders, secrets) lives under ./.kode.
SHELL := /bin/bash
export KODEHUB_HOME ?= $(CURDIR)/.kode
# ArangoDB in the Kind cluster (NodePort 30529 → host 127.0.0.1:18529)
export KODEHUB_ARANGO_URL ?= http://127.0.0.1:18529
export KODEHUB_ARANGO_PASSWORD ?= kodehub
export KODEHUB_DB ?= kodehub
# Optional: a file of env vars (e.g. DEV#_TOKEN for headless agents). Only its *path* is passed to
# the server; scripts/claude-as.sh sources it in its own short-lived process, so the server never
# holds a token. Set KODEHUB_ENV_FILE in a git-ignored local.mk.
-include local.mk
export KODEHUB_ENV_FILE ?=

.PHONY: help db db-down build dev ui-build desktop desktop-build seed check fmt lint test test-unit test-integration app-check ui-test live-test clean kind-up kind-down images deploy k8s-smoke seaweed-check k8s-check up

help:
	@echo "make up       the whole stack in Kind (server + UI on http://127.0.0.1:17878)"
	@echo "make dev      ArangoDB in Kind + kodehub-server on the host (fast dev loop)"
	@echo "make kind-down  delete the Kind cluster"
	@echo "make seed     import examples/auth-poc (server must be running)"
	@echo "make desktop  Tauri desktop app (with make dev running)"
	@echo "make live-test  live agent tests on the Claude Code backend (DEV#_TOKEN by name)"
	@echo "make check    fmt, clippy -D warnings, unit + integration tests, UI typecheck/unit + Playwright e2e"
	@echo "make ui-test  Playwright end-to-end tests of the desktop UI (web build)"

# ArangoDB in the Kind cluster (creates the cluster on first use)
db kind-up:
	@scripts/k8s.sh db

db-down kind-down:
	scripts/k8s.sh down

# Build the server-side image and load it into Kind
images:
	scripts/k8s.sh images

# Deploy every component into Kind and wait for it
deploy:
	scripts/k8s.sh deploy

k8s-smoke:
	scripts/k8s.sh smoke

# The storage service against SeaweedFS (the optional S3 cold tier) in the cluster
seaweed-check:
	scripts/k8s.sh seaweed-check

# The cluster path end to end: image, deploy, smoke test, the S3 tier
k8s-check: images deploy k8s-smoke seaweed-check

# The whole stack in Kind
up: k8s-check
	@echo "Kodehub: http://127.0.0.1:17878"

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

check: fmt lint test app-check ui-test k8s-check
	@echo "make check: all green"

# Live tests with real Claude Code agents (subscription token chosen by env var NAME).
live-test: db build
	KODEHUB_LIVE_TOKEN_ENV=$${KODEHUB_LIVE_TOKEN_ENV:-DEV1_TOKEN} KODEHUB_LIVE_DEMO=1 cargo test -p kodehub-server --test m4_claude_mcp --test m5_live_claude_code -- --nocapture

ui-test: db build
	cd app && npx playwright test

clean:
	cargo clean
