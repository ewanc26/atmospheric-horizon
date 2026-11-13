#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="atmospheric-server:test-$$"
CONTAINER="atmospheric-server-test-$$"
VOLUME="atmospheric-server-test-$$"
TOKEN="test-admin-$$-$(date +%s)"
TMP="$(mktemp -d)"
BODY="$TMP/body"
STATUS=""
PORT=""

cleanup() {
    local status=$?
    trap - EXIT
    if [ "$status" -ne 0 ] && docker inspect "$CONTAINER" >/dev/null 2>&1; then
        echo "--- container logs ---" >&2
        docker logs "$CONTAINER" >&2 || true
    fi
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
    docker volume rm "$VOLUME" >/dev/null 2>&1 || true
    docker image rm "$IMAGE" >/dev/null 2>&1 || true
    rm -rf "$TMP"
    exit "$status"
}
trap cleanup EXIT

start_container() {
    docker run -d \
        --name "$CONTAINER" \
        -e "ATMOSPHERIC_SERVER_TOKEN=$TOKEN" \
        -e ATMOSPHERIC_SERVER_ADDR=0.0.0.0:8787 \
        -v "$VOLUME:/data" \
        -p 127.0.0.1::8787 \
        "$IMAGE" >/dev/null
    PORT="$(docker port "$CONTAINER" 8787/tcp | head -n 1 | sed 's/.*://')"
    for _ in $(seq 1 60); do
        if curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then
            return 0
        fi
        if ! docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null | grep -q true; then
            echo "container exited before becoming healthy" >&2
            return 1
        fi
        sleep 0.5
    done
    echo "container did not become healthy" >&2
    return 1
}

request() {
    local method="$1"
    local path="$2"
    local data="${3:-}"
    local auth="${4:-}"
    local args=(
        -sS
        -o "$BODY"
        -w '%{http_code}'
        -X "$method"
        "http://127.0.0.1:$PORT$path"
    )
    if [ -n "$data" ]; then
        args+=(-H 'content-type: application/json' --data-binary "$data")
    fi
    if [ -n "$auth" ]; then
        args+=(-H "authorization: Bearer $auth")
    fi
    STATUS="$(curl "${args[@]}")"
}

json_get() {
    python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1" < "$BODY"
}

assert_status() {
    local expected="$1"
    local label="$2"
    if [ "$STATUS" != "$expected" ]; then
        echo "FAIL: $label: expected HTTP $expected, got $STATUS" >&2
        cat "$BODY" >&2
        exit 1
    fi
    echo "ok: $label"
}

UUID_1="00000000-0000-4000-8000-000000000001"
UUID_2="00000000-0000-4000-8000-000000000002"
UUID_3="00000000-0000-4000-8000-000000000003"
DID_1="did:plc:alice"
DID_2="did:plc:bob"
DID_3="did:plc:herobrine"

echo "Building $IMAGE"
docker build --tag "$IMAGE" --file "$ROOT/docker/server.Dockerfile" "$ROOT"
docker volume create "$VOLUME" >/dev/null
start_container
echo "Container healthy on 127.0.0.1:$PORT"

request GET /healthz
assert_status 200 "health"
[ "$(json_get status)" = "ok" ]
[ "$(json_get links)" = "0" ]

request PUT "/v1/links/$UUID_1" "{\"player_name\":\"Alex\",\"did\":\"$DID_1\",\"handle\":\"alice.test\"}"
assert_status 401 "link write without token"

request PUT "/v1/links/$UUID_1" "{\"player_name\":\"Alex\",\"did\":\"$DID_1\",\"handle\":\"alice.test\"}" "$TOKEN"
assert_status 201 "admin link write"
[ "$(json_get did)" = "$DID_1" ]

request GET "/v1/links/$UUID_1"
assert_status 200 "public link read"
[ "$(json_get player_uuid)" = "$UUID_1" ]
[ "$(json_get did)" = "$DID_1" ]

request GET /v1/links
assert_status 401 "admin link list without token"
request GET /v1/links "" "$TOKEN"
assert_status 200 "admin link list"
[ "$(json_get total)" = "1" ]

request POST /v1/pairings "{\"player_uuid\":\"$UUID_2\",\"player_name\":\"Steve\",\"ttl_seconds\":120}"
assert_status 401 "pairing creation without token"
request POST /v1/pairings "{\"player_uuid\":\"$UUID_2\",\"player_name\":\"Steve\",\"ttl_seconds\":120}" "$TOKEN"
assert_status 201 "pairing creation"
CODE="$(json_get code)"
[ -n "$CODE" ]

request POST /v1/pairings/claim "{\"code\":\"$CODE\",\"did\":\"$DID_2\",\"handle\":\"bob.test\"}"
assert_status 200 "pairing claim"
[ "$(json_get did)" = "$DID_2" ]
request POST /v1/pairings/claim "{\"code\":\"$CODE\",\"did\":\"$DID_2\",\"handle\":\"bob.test\"}"
assert_status 404 "pairing claim replay"

request POST /v1/pairings "{\"player_uuid\":\"$UUID_3\",\"player_name\":\"Herobrine\"}" "$TOKEN"
assert_status 201 "second pairing creation"
CODE="$(json_get code)"
request POST /v1/pairings/claim "{\"code\":\"$CODE\",\"did\":\"not-a-did\"}"
assert_status 400 "invalid DID claim"
request POST /v1/pairings/claim "{\"code\":\"$CODE\",\"did\":\"$DID_3\"}"
assert_status 200 "claim after validation failure"

docker rm -f "$CONTAINER" >/dev/null
start_container
echo "Container restarted on 127.0.0.1:$PORT"

request GET "/v1/links/$UUID_2"
assert_status 200 "link survives restart"
[ "$(json_get did)" = "$DID_2" ]
request GET /healthz
assert_status 200 "health after restart"
[ "$(json_get links)" = "3" ]

request DELETE "/v1/links/$UUID_1" "" "$TOKEN"
assert_status 204 "admin link delete"
request GET "/v1/links/$UUID_1"
assert_status 404 "deleted link read"

echo "All Docker server tests passed."
