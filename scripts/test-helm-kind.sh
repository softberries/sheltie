#!/usr/bin/env bash
# Installs the Helm chart on a kind cluster with a throwaway Postgres and checks /healthz.
# Usage: scripts/test-helm-kind.sh [image]   (default sheltie:dev, built with deploy/Dockerfile)
# Needs docker, kind, kubectl, helm and openssl. Deletes the cluster when done.
set -euo pipefail

IMAGE="${1:-sheltie:dev}"
CLUSTER="sheltie-chart-test"
NS="sheltie"
WORK="$(mktemp -d)"
trap 'kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT

kind create cluster --name "$CLUSTER" --wait 120s
kind load docker-image "$IMAGE" --name "$CLUSTER"
kubectl create namespace "$NS"

# Throwaway Postgres for the test only.
kubectl -n "$NS" apply -f - <<'YAML'
apiVersion: apps/v1
kind: Deployment
metadata: { name: postgres }
spec:
  selector: { matchLabels: { app: postgres } }
  template:
    metadata: { labels: { app: postgres } }
    spec:
      containers:
        - name: postgres
          image: postgres:17
          env:
            - { name: POSTGRES_USER, value: sheltie }
            - { name: POSTGRES_PASSWORD, value: test }
            - { name: POSTGRES_DB, value: sheltie }
          readinessProbe:
            exec: { command: [pg_isready, -U, sheltie] }
---
apiVersion: v1
kind: Service
metadata: { name: postgres }
spec:
  selector: { app: postgres }
  ports: [{ port: 5432 }]
YAML
kubectl -n "$NS" rollout status deployment/postgres --timeout=180s

openssl genrsa -out "$WORK/key.pem" 2048 2>/dev/null
kubectl -n "$NS" create secret generic sheltie-database \
  --from-literal=url=postgres://sheltie:test@postgres:5432/sheltie
kubectl -n "$NS" create secret generic sheltie-github \
  --from-literal=app-id=1 --from-literal=webhook-secret=test \
  --from-file=private-key.pem="$WORK/key.pem"

helm install sheltie deploy/helm/sheltie -n "$NS" --wait --timeout 180s \
  --set image.repository="${IMAGE%:*}" --set image.tag="${IMAGE##*:}" --set image.pullPolicy=Never

kubectl -n "$NS" port-forward svc/sheltie 18090:80 >/dev/null 2>&1 &
PF=$!
trap 'kill $PF 2>/dev/null || true; kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT
for _ in $(seq 1 30); do
  if curl -sf localhost:18090/healthz; then
    echo
    kubectl -n "$NS" logs deployment/sheltie | grep -q "github=true" && echo "chart test passed"
    exit 0
  fi
  sleep 2
done
kubectl -n "$NS" logs deployment/sheltie || true
echo "chart test failed" >&2
exit 1
