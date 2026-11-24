#!/usr/bin/env bash
# Runs the wsp-hub image on loopback with a throwaway CA: admin init, a second
# admin init refused, serve over TLS, and a verified TLS handshake as the
# compose health check does it. Needs docker and openssl; builds the image
# unless IMAGE names one already built.
#
#   deploy/hub/smoke.sh                 # builds wsp-hub:smoke
#   IMAGE=wsp-hub:latest deploy/hub/smoke.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
image=${IMAGE:-}
if [ -z "$image" ]; then
  image=wsp-hub:smoke
  docker build -t "$image" "$root"
fi

work=$(mktemp -d)
name=wsp-hub-smoke-$$
vol=$name-data
port=${PORT:-17489}
cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$vol" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
fail() { echo "smoke: FAIL: $*" >&2; exit 1; }

# A throwaway CA and a certificate for localhost.
cd "$work"
mkdir etc
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=wsp-smoke-ca" -keyout ca.key -out ca.pem 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout etc/privkey.pem -out leaf.csr 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\n' > san.ext
openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 1 -extfile san.ext -out etc/fullchain.pem 2>/dev/null
cp "$root/deploy/hub/hub.json" etc/hub.json
# The hub runs as uid 10001; a throwaway key may be world-readable.
chmod 644 etc/*

run=(docker run --rm -v "$vol:/var/lib/wsp-hub" -v "$work/etc:/etc/wsp-hub:ro" "$image")

out=$("${run[@]}" admin init --config /etc/wsp-hub/hub.json --admin smoke-admin) || fail "admin init"
grep -q . <<<"$out" || fail "admin init printed nothing"
echo "smoke: admin init ok"
if "${run[@]}" admin init --config /etc/wsp-hub/hub.json --admin another >/dev/null 2>&1; then
  fail "a second admin init was not refused"
fi
echo "smoke: second admin init refused"

docker run -d --name "$name" -p "127.0.0.1:$port:7489" -v "$vol:/var/lib/wsp-hub" -v "$work/etc:/etc/wsp-hub:ro" "$image" >/dev/null
for _ in $(seq 1 50); do
  docker logs "$name" 2>&1 | grep -q 'listening on' && break
  [ "$(docker inspect -f '{{.State.Running}}' "$name")" = true ] || { docker logs "$name" >&2; fail "the hub stopped"; }
  sleep 0.2
done
docker logs "$name" 2>&1 | grep -q 'listening on.*; tls' || { docker logs "$name" >&2; fail "the hub did not start over TLS"; }
echo "smoke: serving"

# Verified against the throwaway CA, by name.
hs=$(openssl s_client -connect "127.0.0.1:$port" -servername localhost -CAfile "$work/ca.pem" \
       -verify_return_error -verify_hostname localhost </dev/null 2>&1) || { echo "$hs" >&2; fail "TLS handshake"; }
grep -q 'Verification: OK\|Verify return code: 0' <<<"$hs" || { echo "$hs" >&2; fail "certificate not verified"; }
echo "smoke: TLS handshake verified"

# The image's own health check command, from inside the container.
docker exec "$name" sh -c 'timeout 5 openssl s_client -connect 127.0.0.1:7489 </dev/null >/dev/null 2>&1' || fail "in-container handshake"
echo "smoke: health check command ok"
echo "smoke: image $image, $(docker image inspect -f '{{.Size}}' "$image" | awk '{printf "%.0f MB", $1/1e6}')"
echo "smoke: PASS"
