# Shared by the identity lab E2E scripts (scripts/identity_*_e2e.sh). Source it after setting
# BASE (SY.admin HTTP), HIVE_ID and WORK_DIR. It holds the SY.admin calls, the test tenants a run
# creates and purges, and the stop and start of the frontdesk that the diags stand in for.

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# admin_call <METHOD> <path under /hives/$HIVE_ID> <out_file> [json_body]: prints the HTTP code.
# `HIVE_ID=<hive> admin_call ...` addresses another hive, such as a replica.
admin_call() {
  local method="$1" path="$2" out="$3" body="${4:-}" code
  if [[ -n "$body" ]]; then
    code="$(curl -sS -o "$out" -w "%{http_code}" -X "$method" "$BASE/hives/$HIVE_ID$path" \
      -H "Content-Type: application/json" -d "$body" || true)"
  else
    code="$(curl -sS -o "$out" -w "%{http_code}" -X "$method" "$BASE/hives/$HIVE_ID$path" || true)"
  fi
  echo "${code:-000}"
}

# json_field <file> <dotted.path>: prints the value, or nothing when it is absent.
json_field() {
  python3 - "$1" "$2" <<'PY'
import json, sys
try:
    cur = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    sys.exit(0)
for key in sys.argv[2].split("."):
    if not isinstance(cur, dict) or key not in cur:
        sys.exit(0)
    cur = cur[key]
if isinstance(cur, bool):
    print("true" if cur else "false")
elif isinstance(cur, (dict, list)):
    print(json.dumps(cur))
elif cur is not None:
    print(cur)
PY
}

# The tenants this run created: the cleanup purges these and no others.
CREATED_TENANTS=()

# create_test_tenant <name> <var>: creates an active tenant through SY.admin and stores its
# tenant_id in <var>. The frontdesk creates no tenants, so the diags cannot either. While SY.admin
# does not answer it retries, until STARTUP_WAIT_SECS (default 0) and every RETRY_SLEEP_SECS. A
# name that matches an existing tenant returns that tenant (created=false): it is refused, so a
# run never registers people into, or purges, a tenant it did not create.
create_test_tenant() {
  local name="$1" var="$2" out="$WORK_DIR/tenant-$1.json" http tenant_id deadline
  deadline=$((SECONDS + ${STARTUP_WAIT_SECS:-0}))
  while :; do
    http="$(admin_call POST /identity/tenants "$out" "{\"name\":\"$name\",\"status\":\"active\"}")"
    if [[ "$http" != "000" ]] || ((SECONDS >= deadline)); then
      break
    fi
    echo "WARN: SY.admin not reachable at $BASE yet, retrying in ${RETRY_SLEEP_SECS:-2}s..."
    sleep "${RETRY_SLEEP_SECS:-2}"
  done
  if [[ "$http" != "200" || "$(json_field "$out" status)" != "ok" ]]; then
    cat "$out" >&2 || true
    fail "SY.admin create_tenant $name answered http=$http"
  fi
  if [[ "$(json_field "$out" payload.created)" != "true" ]]; then
    fail "create_tenant $name matched an existing tenant (created=false); use another test id"
  fi
  tenant_id="$(json_field "$out" payload.tenant_id)"
  [[ "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]] || fail "create_tenant $name returned no tenant_id"
  CREATED_TENANTS+=("$tenant_id")
  printf -v "$var" '%s' "$tenant_id"
  echo "  tenant $name: $tenant_id"
}

# delete_and_purge <path under /hives/$HIVE_ID> <what>: marks, then purges, through SY.admin. It
# warns and never fails, because it runs from the cleanup trap.
delete_and_purge() {
  local path="$1" what="$2" http
  http="$(admin_call DELETE "$path" "$WORK_DIR/cleanup.json")"
  if [[ "$http" == "200" ]]; then
    http="$(admin_call POST "$path/purge" "$WORK_DIR/cleanup.json")"
  fi
  if [[ "$http" == "200" ]]; then
    echo "Cleanup: $what deleted and purged."
  else
    echo "WARN: could not delete+purge $what (http=$http); remove it by hand." >&2
  fi
}

# purge_created_tenants: deletes and purges the tenants this run created, with their ilks.
purge_created_tenants() {
  local tenant_id
  for tenant_id in ${CREATED_TENANTS[@]+"${CREATED_TENANTS[@]}"}; do
    delete_and_purge "/identity/tenants/$tenant_id" "the test tenant $tenant_id"
  done
}

# The diags reach SY.identity under the frontdesk's name, because ILK_REGISTER is only taken from
# the frontdesk. They also use the uuid the frontdesk persisted. So while a diag runs, the router
# sends it the frontdesk's traffic, and when it leaves, a frontdesk that was running stays
# connected but cannot be reached until it reconnects. stop_frontdesk stops the frontdesk for the
# run; start_frontdesk starts it again once the diag is done, and the cleanup trap calls it too in
# case the run failed first.
FRONTDESK_UNIT="${FRONTDESK_UNIT:-sy-frontdesk-gov.service}"
FRONTDESK_STOPPED=0

stop_frontdesk() {
  if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet "$FRONTDESK_UNIT"; then
    systemctl stop "$FRONTDESK_UNIT" || fail "could not stop $FRONTDESK_UNIT"
    FRONTDESK_STOPPED=1
    echo "  $FRONTDESK_UNIT stopped while the diag stands in for it"
  fi
}

start_frontdesk() {
  if [[ "$FRONTDESK_STOPPED" == "1" ]]; then
    FRONTDESK_STOPPED=0
    if systemctl start "$FRONTDESK_UNIT"; then
      echo "  $FRONTDESK_UNIT started again"
    else
      echo "WARN: could not start $FRONTDESK_UNIT; start it by hand." >&2
    fi
  fi
}
