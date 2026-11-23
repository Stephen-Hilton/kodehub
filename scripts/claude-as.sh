#!/bin/sh
# Usage:
#   claude-as.sh <ENV_VAR_NAME | -> <claude args...>
#   claude-as.sh --list-names <regex>
#
# Runs the official `claude` CLI headless. If an env var NAME is given (e.g. DEV2_TOKEN), its value
# is exported as CLAUDE_CODE_OAUTH_TOKEN for this one process. Kodehub only ever passes the
# variable *name*. When KODEHUB_ENV_FILE names a file of env vars, it is sourced HERE, in this
# short-lived process — never in the Kodehub server — so the server never holds a token.
# ANTHROPIC_API_KEY is always removed so Claude Code never bills API credits instead of the
# developer's subscription. `--list-names` prints only variable names, never values.
set -eu
if [ -n "${KODEHUB_ENV_FILE:-}" ] && [ -f "${KODEHUB_ENV_FILE}" ]; then
  set -a
  # shellcheck disable=SC1090
  . "${KODEHUB_ENV_FILE}" >/dev/null 2>&1 || true
  set +a
fi
unset ANTHROPIC_API_KEY
if [ "${1:-}" = "--list-names" ]; then
  pattern="${2:-^DEV[0-9]+_TOKEN$}"
  env | while IFS='=' read -r name value; do
    if [ -n "$value" ] && printf '%s\n' "$name" | grep -Eq "$pattern"; then printf '%s\n' "$name"; fi
  done | sort -u
  exit 0
fi
var="${1:--}"
shift || true
if [ "$var" != "-" ] && [ -n "$var" ]; then
  case "$var" in
    *[!A-Za-z0-9_]*) echo "claude-as: invalid env var name" >&2; exit 64 ;;
    ANTHROPIC_API_KEY) echo "claude-as: refusing ANTHROPIC_API_KEY" >&2; exit 64 ;;
  esac
  eval "tok=\${$var:-}"
  if [ -z "$tok" ]; then echo "claude-as: $var is not set (check KODEHUB_ENV_FILE)" >&2; exit 65; fi
  CLAUDE_CODE_OAUTH_TOKEN="$tok"
  export CLAUDE_CODE_OAUTH_TOKEN
  unset tok
fi
exec "${KODEHUB_CLAUDE_BIN:-claude}" "$@"
