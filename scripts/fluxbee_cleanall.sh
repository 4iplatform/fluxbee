#!/usr/bin/env bash
# fluxbee_cleanall.sh — Wipe everything Fluxbee on the local host.
#
# This is intentionally aggressive and non-interactive. After running it,
# the host has no Fluxbee runtime state, no node configs, no secrets,
# no databases, no vault DB/key, no SHM, no sockets — only your repo at ~/fluxbee.
#
# Then run scripts/install.sh from the repo to bring the system back as
# if installed for the first time. After install you must:
#   - Re-set the OpenAI API key for SY.architect (CONFIG_SET via chat).
#   - Re-fill any IO node *.env files (Slack tokens, etc.).
#
# Postgres databases (fluxbee, fluxbee_identity, fluxbee_storage) are dropped only
# when the configured database URL points at the local host. A remote URL
# aborts the Postgres step — never wipe somebody else's database by accident.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE_DIR="${STATE_DIR:-/var/lib/fluxbee}"
RUN_DIR="${RUN_DIR:-/var/run/fluxbee}"
CONFIG_DIR="${CONFIG_DIR:-/etc/fluxbee}"

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
  $SUDO -v
fi

step() { echo "[cleanall] $*"; }

run_as_postgres() {
  if command -v sudo >/dev/null 2>&1; then
    sudo -u postgres sh -c 'cd /tmp && exec "$@"' sh "$@"
  elif command -v runuser >/dev/null 2>&1; then
    runuser -u postgres -- sh -c 'cd /tmp && exec "$@"' sh "$@"
  else
    echo "  warning: neither sudo nor runuser is available to execute as OS user 'postgres'"
    return 1
  fi
}

# ── 1. Stop services + residual processes ──────────────────────────────────
step "Stopping services..."
if [[ -x "$SCRIPT_DIR/fluxbee_stop.sh" ]]; then
  "$SCRIPT_DIR/fluxbee_stop.sh" || true
else
  echo "  warning: fluxbee_stop.sh not found at $SCRIPT_DIR; assuming services already stopped"
fi

# ── 2. Resolve Postgres URL BEFORE we wipe configs/node secrets ──────────────
read_json_secret_value() {
  local path="$1"
  local expr="$2"
  [[ -f "$path" ]] || return 1
  if [[ -r "$path" ]]; then
    jq -r "$expr" "$path" 2>/dev/null
  else
    $SUDO cat "$path" 2>/dev/null | jq -r "$expr" 2>/dev/null
  fi
}

resolve_database_url() {
  if [[ -n "${FLUXBEE_DATABASE_URL:-}" ]]; then
    echo "$FLUXBEE_DATABASE_URL"
    return
  fi
  if [[ -n "${JSR_DATABASE_URL:-}" ]]; then
    echo "$JSR_DATABASE_URL"
    return
  fi
  local secrets="$CONFIG_DIR/secrets.json"
  if [[ -f "$secrets" ]] && command -v jq >/dev/null 2>&1; then
    local url
    url="$(read_json_secret_value "$secrets" '.fluxbee_database_url // .database_url // empty' || true)"
    if [[ -n "$url" ]]; then
      echo "$url"
      return
    fi
  fi
  if command -v jq >/dev/null 2>&1; then
    local candidate url
    for candidate in \
      "$STATE_DIR/nodes/SY/SY.identity@motherbee/secrets.json" \
      "$STATE_DIR/nodes/SY/SY.storage@motherbee/secrets.json" \
      "$STATE_DIR"/nodes/SY/SY.identity@*/secrets.json \
      "$STATE_DIR"/nodes/SY/SY.storage@*/secrets.json; do
      [[ -f "$candidate" ]] || continue
      url="$(read_json_secret_value "$candidate" '.secrets.postgres_url // empty' || true)"
      if [[ -n "$url" ]]; then
        echo "$url"
        return
      fi
    done
  fi
}

is_local_postgres() {
  local url="$1"
  [[ -n "$url" ]] || return 1
  if [[ "$url" =~ @localhost(:|/) ]] || \
     [[ "$url" =~ @127\.0\.0\.1(:|/) ]] || \
     [[ "$url" =~ @\[::1\](:|/) ]] || \
     [[ "$url" =~ @/ ]]; then
    return 0
  fi
  return 1
}

DB_URL="$(resolve_database_url)"

drop_local_fluxbee_database() {
  local db="$1"
  step "DROP DATABASE IF EXISTS $db"
  run_as_postgres psql -v ON_ERROR_STOP=1 -d postgres \
    -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '$db' AND pid <> pg_backend_pid();" \
    -c "DROP DATABASE IF EXISTS \"$db\";" >/dev/null 2>&1 \
    || echo "  warning: drop of $db failed"
}

# ── 3. SHM (all known prefixes + custom shm referenced from identity.yaml) ──
step "Removing SHM regions..."
for pattern in \
  "/dev/shm/jsr-config-"* \
  "/dev/shm/jsr-lsa-"* \
  "/dev/shm/jsr-identity-"* \
  "/dev/shm/jsr-opa-"* \
  "/dev/shm/jsr-memory-"*; do
  for path in $pattern; do
    [[ -e "$path" ]] || continue
    $SUDO rm -f "$path" || true
  done
done

# Custom SHM names referenced in any state/<hive>/identity.yaml (must run BEFORE state wipe)
if [[ -d "$STATE_DIR/state" ]]; then
  while IFS= read -r identity_path; do
    [[ -n "$identity_path" ]] || continue
    shm_name="$(
      awk '
        /^shm:/ { in_shm=1; next }
        in_shm && /^[^[:space:]]/ { in_shm=0 }
        in_shm && /^[[:space:]]*name:/ {
          value=$2
          gsub(/"/, "", value)
          print value
          exit
        }
      ' "$identity_path"
    )"
    [[ -n "$shm_name" ]] || continue
    $SUDO rm -f "/dev/shm/${shm_name#/}" || true
  done < <(find "$STATE_DIR/state" -mindepth 2 -maxdepth 2 -type f -name identity.yaml 2>/dev/null | sort)
fi

# ── 4. Router sockets ──────────────────────────────────────────────────────
step "Removing router sockets..."
$SUDO find "$RUN_DIR/routers" -maxdepth 1 \( -type s -o -type f \) -name '*.sock' -delete 2>/dev/null || true

# ── 5. Vault: explicit wipe of secrets DB + master key ────────────────────
# Phase J / J' contract: every secret in the cluster lives in vault.db
# (AES-256-GCM at rest, encrypted with vault.master.key). Deleting both is
# the only safe way to start clean — there is no separate "list and delete
# every secret" path because by spec vault is the canonical and only secrets
# store, and `fluxbee_cleanall.sh` is the canonical reset.
VAULT_DB="$STATE_DIR/vault.db"
VAULT_KEY="$CONFIG_DIR/vault.master.key"
if [[ -e "$VAULT_DB" || -e "$VAULT_KEY" ]]; then
  step "Removing vault secrets store (vault.db + vault.master.key)"
  $SUDO rm -f "$VAULT_DB" "$VAULT_KEY" 2>/dev/null || true
fi

# ── 6. Wipe entire $STATE_DIR (every subdir: state/, nodes/, vendor/, dist/, ssh/, blob/, ...) ──
if [[ -d "$STATE_DIR" ]]; then
  step "Wiping contents of $STATE_DIR (everything: state, nodes, vendor, dist, ssh, blob, ...)"
  $SUDO find "$STATE_DIR" -mindepth 1 -delete 2>/dev/null || true
fi

# ── 7. Wipe entire $CONFIG_DIR (hive.yaml, handbook, *.env, secrets.json, ...) ──
if [[ -d "$CONFIG_DIR" ]]; then
  step "Wiping contents of $CONFIG_DIR (hive.yaml, handbook, *.env, secrets, ...)"
  $SUDO find "$CONFIG_DIR" -mindepth 1 -delete 2>/dev/null || true
fi

# ── 8. Postgres — drop only if URL points at localhost ────────────────────
step "Checking Postgres URL..."
if [[ -z "$DB_URL" ]]; then
  echo "  no FLUXBEE_DATABASE_URL / JSR_DATABASE_URL / secrets.json database_url found"
  echo "  attempting drop assuming local Postgres anyway (will silently no-op if not present)"
  if command -v psql >/dev/null 2>&1; then
    for db in fluxbee fluxbee_identity fluxbee_storage; do
      drop_local_fluxbee_database "$db"
    done
  else
    echo "  psql not in PATH — skipping Postgres drop"
  fi
elif ! is_local_postgres "$DB_URL"; then
  echo "  Postgres URL is not local (host is remote) — refusing to drop. Skipping Postgres step."
  echo "  If you really want to drop the remote DB, do it manually."
else
  if ! command -v psql >/dev/null 2>&1; then
    echo "  warning: psql not in PATH — skipping Postgres drop"
  else
    for db in fluxbee fluxbee_identity fluxbee_storage; do
      drop_local_fluxbee_database "$db"
    done
  fi
fi

# ── 9. Done ────────────────────────────────────────────────────────────────
step "Cleanall complete. Host has no Fluxbee state."
echo
echo "Next steps:"
echo "  1. cd ~/fluxbee   # or wherever your repo lives"
echo "  2. sudo ./scripts/install.sh"
echo "  3. After install completes, set the OpenAI API key for SY.architect"
echo "     (CONFIG_SET via chat: POST /architect/control/config-set)."
echo "  4. If you have IO nodes (Slack, etc.), run the appropriate install-io*.sh"
echo "     and edit the regenerated *.env files in $CONFIG_DIR/ to fill in tokens."
