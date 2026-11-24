#!/usr/bin/env bash
# Kind cluster lifecycle for Kodehub (spec §3: Kubernetes everywhere, Kind on a laptop).
#   scripts/k8s.sh up        create the cluster (if missing), secrets, ArangoDB
#   scripts/k8s.sh images    build kodehub/kodehub:dev and load it (and arangodb:3.12) into Kind
#   scripts/k8s.sh deploy    apply every manifest and wait for the stack
#   scripts/k8s.sh smoke     health of server, storage and the DB through the host ports
#   scripts/k8s.sh seaweed-check   run the storage service's S3 test against in-cluster SeaweedFS
#   scripts/k8s.sh down      delete the cluster
# KODEHUB_STORAGE_BACKEND=seaweedfs (on deploy) runs storage in hybrid mode with SeaweedFS as
# the cold tier; the default, local, keeps every object on the storage service's own volume.
set -euo pipefail
CLUSTER=kodehub
NS=kodehub
CTX="kind-$CLUSTER"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
K="kubectl --context $CTX"
SECRETS="${KODEHUB_HOME:-$ROOT/.kode}/k8s-secrets.env"
SEAWEED_IMAGE=chrislusf/seaweedfs:3.97

ensure_cluster() {
  if ! kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
    # kind switches the current kubectl context; put the developer's back afterwards
    local prev; prev=$(kubectl config current-context 2>/dev/null || true)
    kind create cluster --config "$ROOT/deploy/kind/cluster.yaml" --wait 120s
    if [ -n "$prev" ]; then kubectl config use-context "$prev" >/dev/null; fi
  fi
  $K apply -f "$ROOT/deploy/k8s/00-namespace.yaml" >/dev/null
  # secrets are generated once per laptop and never committed
  mkdir -p "$(dirname "$SECRETS")"
  if [ ! -f "$SECRETS" ]; then
    umask 077
    {
      echo "arango-password=${KODEHUB_ARANGO_PASSWORD:-kodehub}"
      echo "secret-key=$(head -c 32 /dev/urandom | base64)"
      echo "storage-token=$(head -c 24 /dev/urandom | base64 | tr -d '/+=')"
    } > "$SECRETS"
  fi
  # added after the first release of this script: S3 keys for the optional SeaweedFS tier
  if ! grep -q '^s3-access-key=' "$SECRETS"; then
    ( umask 077
      echo "s3-access-key=kodehub$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
      echo "s3-secret-key=$(head -c 30 /dev/urandom | base64 | tr -d '/+=')" ) >> "$SECRETS"
  fi
  $K -n $NS create secret generic kodehub-secrets --from-env-file="$SECRETS" \
    --dry-run=client -o yaml | $K apply -f - >/dev/null
}

load_image() {
  local img="$1"
  # skip when the node already has this exact image id
  local want have
  want=$(docker image inspect --format '{{.Id}}' "$img")
  have=$(docker exec "$CLUSTER-control-plane" crictl inspecti -o go-template --template '{{.status.id}}' "$img" 2>/dev/null || true)
  if [ "$want" != "$have" ]; then
    # `kind load` imports --all-platforms, which fails for single-platform pulls of multi-arch
    # images; import just this platform through containerd instead
    docker save "$img" | docker exec -i "$CLUSTER-control-plane" \
      ctr --namespace=k8s.io images import --snapshotter=overlayfs - >/dev/null
  fi
}

db() {
  ensure_cluster
  load_image arangodb:3.12
  $K apply -f "$ROOT/deploy/k8s/10-arangodb.yaml" >/dev/null
  $K -n $NS rollout status statefulset/arangodb --timeout=240s >/dev/null
  for _ in $(seq 1 90); do
    curl -sf -u "root:${KODEHUB_ARANGO_PASSWORD:-kodehub}" http://127.0.0.1:18529/_api/version >/dev/null && return 0
    sleep 1
  done
  echo "ArangoDB in Kind is not reachable on 127.0.0.1:18529" >&2; exit 1
}

images() {
  ensure_cluster
  if [ -d "$ROOT/app/node_modules" ]; then (cd "$ROOT/app" && npm run --silent build >/dev/null); fi
  mkdir -p "$ROOT/app/dist"
  DOCKER_BUILDKIT=1 docker build -q -f "$ROOT/deploy/images/kodehub.Dockerfile" -t kodehub/kodehub:dev "$ROOT" >/dev/null
  # test container types: one core layer, language layers on top (spec §5.4.1)
  for t in core rust python; do
    DOCKER_BUILDKIT=1 docker build -q -f "$ROOT/deploy/images/test-$t.Dockerfile" -t "kodehub/test-$t:dev" "$ROOT/deploy/images" >/dev/null
  done
  # the one small agent base image (spec §4.1)
  DOCKER_BUILDKIT=1 docker build -q -f "$ROOT/deploy/images/agent.Dockerfile" -t kodehub/agent:dev "$ROOT/deploy/images" >/dev/null
  # speech transcription: a local Whisper model (spec §14.1)
  DOCKER_BUILDKIT=1 docker build -q -f "$ROOT/deploy/images/transcribe.Dockerfile" -t kodehub/transcribe:dev "$ROOT/deploy/images" >/dev/null
  for img in kodehub/kodehub:dev kodehub/test-rust:dev kodehub/test-python:dev kodehub/agent:dev kodehub/transcribe:dev arangodb:3.12; do load_image "$img"; done
}

images_browser() {
  ensure_cluster
  DOCKER_BUILDKIT=1 docker build -q -f "$ROOT/deploy/images/test-browser.Dockerfile" -t kodehub/test-browser:dev "$ROOT/deploy/images" >/dev/null
  load_image kodehub/test-browser:dev
}

secret_val() { grep "^$1=" "$SECRETS" | cut -d= -f2-; }

# SeaweedFS in single-server mode, its S3 identity from the generated keys
seaweed() {
  ensure_cluster
  docker image inspect "$SEAWEED_IMAGE" >/dev/null 2>&1 || docker pull -q "$SEAWEED_IMAGE" >/dev/null
  load_image "$SEAWEED_IMAGE"
  local cfg; cfg=$(python3 -c 'import json,sys;print(json.dumps({"identities":[{"name":"kodehub","credentials":[{"accessKey":sys.argv[1],"secretKey":sys.argv[2]}],"actions":["Admin","Read","Write","List","Tagging"]}]}))' \
    "$(secret_val s3-access-key)" "$(secret_val s3-secret-key)")
  $K -n $NS create secret generic seaweedfs-s3 --from-literal=s3.json="$cfg" \
    --dry-run=client -o yaml | $K apply -f - >/dev/null
  $K apply -f "$ROOT/deploy/k8s/optional/seaweedfs.yaml" >/dev/null
  $K -n $NS rollout status deployment/seaweedfs --timeout=180s >/dev/null
}

# Which object store backs the storage service (D-054): local (default) or seaweedfs
storage_profile() {
  case "${KODEHUB_STORAGE_BACKEND:-local}" in
    local)
      $K -n $NS create configmap storage-profile --from-literal=KODEHUB_STORAGE_MODE=local \
        --dry-run=client -o yaml | $K apply -f - >/dev/null
      $K -n $NS delete secret storage-upstream --ignore-not-found >/dev/null ;;
    seaweedfs)
      seaweed
      $K -n $NS create configmap storage-profile \
        --from-literal=KODEHUB_STORAGE_MODE=hybrid \
        --from-literal=KODEHUB_STORAGE_UPSTREAM_URL=http://seaweedfs-s3.$NS.svc:8333 \
        --from-literal=KODEHUB_STORAGE_UPSTREAM_AUTH=sigv4 \
        --from-literal=KODEHUB_STORAGE_UPSTREAM_BUCKET=kodehub-cold \
        --dry-run=client -o yaml | $K apply -f - >/dev/null
      $K -n $NS create secret generic storage-upstream \
        --from-literal=KODEHUB_STORAGE_UPSTREAM_ACCESS_KEY="$(secret_val s3-access-key)" \
        --from-literal=KODEHUB_STORAGE_UPSTREAM_SECRET_KEY="$(secret_val s3-secret-key)" \
        --dry-run=client -o yaml | $K apply -f - >/dev/null ;;
    *) echo "KODEHUB_STORAGE_BACKEND must be local or seaweedfs" >&2; exit 2 ;;
  esac
}

# The storage service's S3 integration test against the in-cluster SeaweedFS, through a
# port-forward (the Kind cluster exposes no SeaweedFS host port)
seaweed_check() {
  seaweed
  local port=18334 pf
  $K -n $NS port-forward svc/seaweedfs-s3 $port:8333 >/dev/null 2>&1 &
  pf=$!
  trap 'kill $pf 2>/dev/null || true' RETURN
  for _ in $(seq 1 30); do curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && break; sleep 1; done
  KODEHUB_TEST_S3_URL="http://127.0.0.1:$port" \
  KODEHUB_TEST_S3_ACCESS_KEY="$(secret_val s3-access-key)" \
  KODEHUB_TEST_S3_SECRET_KEY="$(secret_val s3-secret-key)" \
    cargo test -q -p kodehub-storage --test s5_seaweedfs
  echo "seaweedfs: the storage service's S3 path works against SeaweedFS in the cluster"
}

deploy() {
  db
  storage_profile
  $K apply -f "$ROOT/deploy/k8s/" >/dev/null
  # pick up a freshly loaded image
  $K -n $NS rollout restart deployment/storage deployment/kodehub-server >/dev/null
  $K -n $NS rollout status deployment/storage --timeout=180s >/dev/null
  $K -n $NS rollout status deployment/kodehub-server --timeout=300s >/dev/null
  if $K -n $NS get deployment/kodehub-scheduler >/dev/null 2>&1; then
    $K -n $NS rollout restart deployment/kodehub-scheduler >/dev/null
    $K -n $NS rollout status deployment/kodehub-scheduler --timeout=180s >/dev/null
  fi
}

smoke() {
  local fail=0
  # NodePort routes settle a few seconds after a rollout
  for _ in $(seq 1 60); do
    curl -sf http://127.0.0.1:17878/health >/dev/null && curl -sf http://127.0.0.1:19000/health >/dev/null && break
    sleep 1
  done
  curl -sf http://127.0.0.1:17878/health | grep -q '"connected":true' || { echo "server /health failed" >&2; fail=1; }
  curl -sf http://127.0.0.1:19000/health | grep -q '"status":"ok"' || { echo "storage /health failed" >&2; fail=1; }
  curl -sf -u "root:${KODEHUB_ARANGO_PASSWORD:-kodehub}" http://127.0.0.1:18529/_api/version >/dev/null || { echo "arangodb unreachable" >&2; fail=1; }
  # an object round-trips through the in-cluster storage service
  local tok; tok=$(grep '^storage-token=' "$SECRETS" | cut -d= -f2-)
  curl -sf -X PUT -H "Authorization: Bearer $tok" --data-binary "smoke-$$" http://127.0.0.1:19000/kodehub/projects/smoke/object >/dev/null \
    && [ "$(curl -sf -H "Authorization: Bearer $tok" http://127.0.0.1:19000/kodehub/projects/smoke/object)" = "smoke-$$" ] \
    || { echo "storage round trip failed" >&2; fail=1; }
  # hybrid mode: a backup of that project lands in the cold tier (SeaweedFS)
  if [ "$($K -n $NS get configmap storage-profile -o jsonpath='{.data.KODEHUB_STORAGE_MODE}' 2>/dev/null)" = hybrid ]; then
    curl -sf -X POST -H "Authorization: Bearer $tok" "http://127.0.0.1:19000/_backup?project=smoke" >/dev/null \
      && curl -sf -H "Authorization: Bearer $tok" "http://127.0.0.1:19000/_backups?project=smoke" | grep -q 'backups/smoke/' \
      && echo "storage: hybrid backup reached the cold tier" \
      || { echo "hybrid backup to the cold tier failed" >&2; fail=1; }
  fi
  # the UI is served, and the API demands a token
  curl -sf http://127.0.0.1:17878/ | grep -qi '<div id="root"' || { echo "UI not served" >&2; fail=1; }
  [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:17878/api/map)" = "401" ] || { echo "API not protected" >&2; fail=1; }
  if $K -n $NS get deployment/kodehub-scheduler >/dev/null 2>&1; then
    $K -n $NS get deployment/kodehub-scheduler -o jsonpath='{.status.readyReplicas}' | grep -q '[1-9]' || { echo "scheduler not ready" >&2; fail=1; }
  fi
  [ $fail = 0 ] || return $fail
  e2e
  echo "k8s smoke: ok"
}

# End to end in the cluster: sign up, create a project, seed it, press "Test now", and wait
# for the scheduler → orchestrator → build Job → test Jobs chain to record formal results.
e2e() {
  local api=http://127.0.0.1:17878 user="smoke$(date +%s)" tok proj item state
  tok=$(curl -sf -X POST -H 'content-type: application/json' \
    -d "{\"username\":\"$user\",\"password\":\"smoke-password-1\"}" $api/api/auth/register | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')
  proj=$(curl -sf -X POST -H "Authorization: Bearer $tok" -H 'content-type: application/json' \
    -d '{"name":"smoke"}' $api/api/projects | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')
  local H=(-H "Authorization: Bearer $tok" -H "x-kodehub-project: $proj" -H 'content-type: application/json')
  curl -sf -X POST "${H[@]}" -d '{"path":"/app/examples/auth-poc"}' $api/api/seed >/dev/null
  curl -sf -X POST "${H[@]}" -d '{"target":"project"}' $api/api/test-now >/dev/null
  for _ in $(seq 1 300); do
    item=$(curl -sf "${H[@]}" "$api/api/work-items" | python3 -c '
import sys,json
items=[w for w in json.load(sys.stdin) if w["agent_type"]=="test_run"]
print(json.dumps(items[0]) if items else "")')
    state=$(echo "$item" | python3 -c 'import sys,json;d=sys.stdin.read().strip();print(json.loads(d)["state"] if d else "")')
    case "$state" in
      done) break ;;
      failed|cancelled) echo "in-cluster test run $state: $item" >&2; return 1 ;;
    esac
    sleep 2
  done
  echo "$item" | python3 -c '
import sys,json
w=json.load(sys.stdin); r=w.get("result") or {}
assert w["state"]=="done", w["state"]
assert r.get("runtime")=="kubernetes", r
assert r.get("tests")==9 and r.get("failed")==0, r
print("in-cluster: %d tests passed in containers (%d bundles built)" % (r["passed"], r["bundles_built"]))'
  # an agent task in its own container (recorded backend: no model credential needed)
  curl -sf -X PUT "${H[@]}" -d '{"value":"recorded"}' "$api/api/scopes/project:$proj/settings/agent_backend" >/dev/null
  curl -sf -X POST "${H[@]}" -d '{"agent_type":"planning","kind":"smoke","target":"philosophy/philosophy"}' $api/api/work-items >/dev/null
  for _ in $(seq 1 120); do
    item=$(curl -sf "${H[@]}" "$api/api/work-items" | python3 -c '
import sys,json
items=[w for w in json.load(sys.stdin) if w["kind"]=="smoke"]
print(json.dumps(items[0]) if items else "")')
    state=$(echo "$item" | python3 -c 'import sys,json;d=sys.stdin.read().strip();print(json.loads(d)["state"] if d else "")')
    case "$state" in done|failed|blocked) break ;; esac
    sleep 2
  done
  echo "$item" | python3 -c '
import sys,json
w=json.load(sys.stdin)
assert w["state"]=="done", w
assert (w.get("result") or {}).get("backend")=="recorded-in-container", w.get("result")
print("in-cluster: an agent task ran in its own container")'
  # speech to text in a transcription container (macOS `say` makes the audio)
  if command -v say >/dev/null; then
    local wav; wav="$(mktemp -d)/s.wav"
    say -o "$wav" --data-format=LEI16@16000 "Checkout never loses an order."
    curl -sf -X POST -H "Authorization: Bearer $tok" -H "x-kodehub-project: $proj" -H 'content-type: audio/wav' \
      --data-binary @"$wav" $api/api/transcribe | python3 -c '
import sys,json
t=json.load(sys.stdin)["text"].lower()
assert "order" in t, t
print("in-cluster: transcribed speech: %r" % t)'
  fi
}

case "${1:-}" in
  up) db ;;
  db) db ;;
  images) images ;;
  images-browser) images_browser ;;
  deploy) deploy ;;
  smoke) smoke ;;
  seaweed-check) seaweed_check ;;
  down) kind delete cluster --name "$CLUSTER" ;;
  *) echo "usage: $0 up|db|images|deploy|smoke|seaweed-check|down" >&2; exit 2 ;;
esac
