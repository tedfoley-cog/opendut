#!/usr/bin/env bash

set -euo pipefail

image=${1:-opendut-nginx-webdav:test}
container="nginx-webdav-test-$$"
cleanup() {
    docker rm --force "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker run --detach --rm --name "$container" --publish 127.0.0.1::80 "$image" >/dev/null
port=$(docker port "$container" 80/tcp | sed -E 's/.*:([0-9]+)$/\1/')

body=$(mktemp)
trap 'rm -f "$body"; cleanup' EXIT

printf '%s' '<?xml version="1.0"?><propfind xmlns="DAV:"><propname/></propfind>' >"$body"
small_status=$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
    --request PROPFIND --header 'Content-Type: application/xml' \
    --data-binary @"$body" "http://127.0.0.1:$port/")
test "$small_status" = 207

{
    printf '%s' '<?xml version="1.0"?><propfind xmlns="DAV:"><propname>'
    head -c 16384 /dev/zero | tr '\0' x
    printf '%s' '</propname></propfind>'
} >"$body"
large_status=$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
    --request PROPFIND --header 'Content-Type: application/xml' \
    --data-binary @"$body" "http://127.0.0.1:$port/")
test "$large_status" = 413

printf 'PROPFIND small body: %s; oversized body: %s\n' "$small_status" "$large_status"
