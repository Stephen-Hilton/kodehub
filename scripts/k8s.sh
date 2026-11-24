#!/usr/bin/env bash
# Kind cluster lifecycle for Kodehub (spec §3: Kubernetes everywhere, Kind on a laptop).
#   scripts/k8s.sh up        create the cluster (if missing), secrets, ArangoDB
#   scripts/k8s.sh images    build kodehub/kodehub:dev and load it (and arangodb:3.12) into Kind
#   scripts/k8s.sh deploy    apply every manifest and wait for the stack
#   scripts/k8s.sh smoke     health of server, storage and the DB through the host ports
#   scripts/k8s.sh down      delete the cluster
set -euo pipefail
CLUSTER=kodehub
NS=kodehub
CTX="kind-$CLUSTER"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
K="kubectl --context $CTX"
SECRETS="${KODEHUB_HOME:-$ROOT/.kode}/k8s-secrets.env"

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
  load_image kodehub/kodehub:dev
  load_image arangodb:3.12
}

deploy() {
  db
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
  # the UI is served, and the API demands a token
  curl -sf http://127.0.0.1:17878/ | grep -qi '<div id="root"' || { echo "UI not served" >&2; fail=1; }
  [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:17878/api/map)" = "401" ] || { echo "API not protected" >&2; fail=1; }
  if $K -n $NS get deployment/kodehub-scheduler >/dev/null 2>&1; then
    $K -n $NS get deployment/kodehub-scheduler -o jsonpath='{.status.readyReplicas}' | grep -q '[1-9]' || { echo "scheduler not ready" >&2; fail=1; }
  fi
  [ $fail = 0 ] && echo "k8s smoke: ok"
  return $fail
}

case "${1:-}" in
  up) db ;;
  db) db ;;
  images) images ;;
  deploy) deploy ;;
  smoke) smoke ;;
  down) kind delete cluster --name "$CLUSTER" ;;
  *) echo "usage: $0 up|db|images|deploy|smoke|down" >&2; exit 2 ;;
esac
