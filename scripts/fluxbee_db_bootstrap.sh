#!/usr/bin/env bash
set -euo pipefail

# Local PostgreSQL bootstrap for Fluxbee package/install flows.
#
# Responsibilities:
# - ensure local PostgreSQL roles/databases exist
# - optionally reset Fluxbee databases for clean test installs
#
# Non-responsibilities:
# - does not write Fluxbee node secrets
# - does not start/stop Fluxbee services

RESET="${FLUXBEE_DB_RESET:-0}"
DB_USER="${FLUXBEE_DB_USER:-}"
DB_PASSWORD="${FLUXBEE_DB_PASSWORD:-}"
DB_NAMES="${FLUXBEE_DB_NAMES:-fluxbee fluxbee_identity fluxbee_storage}"

usage() {
  cat <<'EOF'
Usage: scripts/fluxbee_db_bootstrap.sh [options]

Options:
  --reset                 Drop and recreate Fluxbee databases.
  --user <role>           PostgreSQL role that should own the databases.
  --password <password>   Set/update role password.
  --db <name>             Add one database to manage. Can be repeated.
  -h, --help              Show this help.

Environment:
  FLUXBEE_DB_RESET=1      Same as --reset.
  FLUXBEE_DB_USER=sa      Owner role. If omitted: existing 'sa', else existing
                          'fluxbee', else create/use 'fluxbee'.
  FLUXBEE_DB_PASSWORD=... Optional password for the owner role.
  FLUXBEE_DB_NAMES="..."  Space-separated DB list. Default:
                          fluxbee fluxbee_identity fluxbee_storage.
EOF
}

explicit_dbs=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --reset)
      RESET=1
      shift
      ;;
    --user)
      DB_USER="${2:-}"
      shift 2
      ;;
    --password)
      DB_PASSWORD="${2:-}"
      shift 2
      ;;
    --db)
      explicit_dbs+=("${2:-}")
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "${#explicit_dbs[@]}" -gt 0 ]]; then
  DB_NAMES="${explicit_dbs[*]}"
fi

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
  $SUDO -v
fi

step() { echo "[db-bootstrap] $*"; }

run_as_postgres() {
  if command -v sudo >/dev/null 2>&1; then
    sudo -u postgres sh -c 'cd /tmp && exec "$@"' sh "$@"
  elif command -v runuser >/dev/null 2>&1; then
    runuser -u postgres -- sh -c 'cd /tmp && exec "$@"' sh "$@"
  else
    echo "Error: neither sudo nor runuser is available to execute as OS user 'postgres'." >&2
    exit 1
  fi
}

require_ident() {
  local kind="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    echo "Error: invalid PostgreSQL $kind identifier: $value" >&2
    exit 2
  fi
}

sql_literal() {
  printf "%s" "$1" | sed "s/'/''/g"
}

psql_postgres() {
  run_as_postgres psql -d postgres -v ON_ERROR_STOP=1 "$@"
}

role_exists() {
  local role="$1"
  [[ "$(psql_postgres -tAc "SELECT 1 FROM pg_roles WHERE rolname = '$(sql_literal "$role")';" | tr -d '[:space:]')" == "1" ]]
}

database_exists() {
  local db="$1"
  [[ "$(psql_postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '$(sql_literal "$db")';" | tr -d '[:space:]')" == "1" ]]
}

if ! command -v psql >/dev/null 2>&1; then
  echo "Error: psql not found. Install PostgreSQL client/server packages first." >&2
  exit 1
fi

if ! id postgres >/dev/null 2>&1; then
  echo "Error: local OS user 'postgres' not found. Is PostgreSQL installed locally?" >&2
  exit 1
fi

if ! psql_postgres -tAc "SELECT 1;" >/dev/null; then
  echo "Error: cannot connect to local PostgreSQL as OS user 'postgres'." >&2
  exit 1
fi

if [[ -z "$DB_USER" ]]; then
  if role_exists "sa"; then
    DB_USER="sa"
  elif role_exists "fluxbee"; then
    DB_USER="fluxbee"
  else
    DB_USER="fluxbee"
  fi
fi

require_ident "role" "$DB_USER"
for db in $DB_NAMES; do
  require_ident "database" "$db"
done

if role_exists "$DB_USER"; then
  step "Using PostgreSQL role '$DB_USER'"
  psql_postgres -c "ALTER ROLE \"$DB_USER\" LOGIN;" >/dev/null
  if [[ -n "$DB_PASSWORD" ]]; then
    escaped_password="$(sql_literal "$DB_PASSWORD")"
    psql_postgres -c "ALTER ROLE \"$DB_USER\" WITH PASSWORD '$escaped_password';" >/dev/null
    step "Updated password for role '$DB_USER'"
  fi
else
  if [[ -n "$DB_PASSWORD" ]]; then
    escaped_password="$(sql_literal "$DB_PASSWORD")"
    psql_postgres -c "CREATE ROLE \"$DB_USER\" LOGIN PASSWORD '$escaped_password';" >/dev/null
  else
    psql_postgres -c "CREATE ROLE \"$DB_USER\" LOGIN;" >/dev/null
  fi
  step "Created PostgreSQL role '$DB_USER'"
fi

for db in $DB_NAMES; do
  if [[ "$RESET" == "1" ]]; then
    step "Resetting database '$db'"
    psql_postgres \
      -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '$db' AND pid <> pg_backend_pid();" \
      -c "DROP DATABASE IF EXISTS \"$db\";" >/dev/null
  fi

  if database_exists "$db"; then
    step "Database '$db' already exists"
    psql_postgres -c "ALTER DATABASE \"$db\" OWNER TO \"$DB_USER\";" >/dev/null
  else
    step "Creating database '$db' owned by '$DB_USER'"
    psql_postgres -c "CREATE DATABASE \"$db\" OWNER \"$DB_USER\";" >/dev/null
  fi

  run_as_postgres psql -d "$db" -v ON_ERROR_STOP=1 \
    -c "ALTER SCHEMA public OWNER TO \"$DB_USER\";" \
    -c "GRANT ALL ON SCHEMA public TO \"$DB_USER\";" >/dev/null
done

step "PostgreSQL bootstrap complete. Managed DBs: $DB_NAMES owner=$DB_USER reset=$RESET"
if [[ -n "$DB_PASSWORD" ]]; then
  step "Base URL for CONFIG_SET secrets: postgresql://$DB_USER:***@127.0.0.1:5432/fluxbee"
else
  step "No password was set. If node secrets use TCP auth, set FLUXBEE_DB_PASSWORD or alter the role manually."
fi
