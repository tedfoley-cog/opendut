#!/usr/bin/env bash

set -euo pipefail

image=${1:-opendut-nginx-webdav:test}
traefik_image=${2:-traefik:v3.6.2}
network="nginx-webdav-test-$$"
server="nginx-webdav-test-server-$$"
proxy="nginx-webdav-test-proxy-$$"
cleanup() {
    docker rm --force "$proxy" "$server" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker network create "$network" >/dev/null
docker run --detach --rm --name "$server" --network "$network" \
    --label traefik.enable=true \
    --label 'traefik.http.routers.nginx-webdav.rule=Host(`webdav.test`)' \
    --label traefik.http.routers.nginx-webdav.entrypoints=web \
    --label traefik.http.routers.nginx-webdav.service=nginx-webdav \
    --label traefik.http.services.nginx-webdav.loadbalancer.server.port=80 \
    --label 'traefik.http.middlewares.nginx-webdav-propfind-limit.buffering.maxRequestBodyBytes=16384' \
    --label 'traefik.http.routers.nginx-webdav-propfind.rule=Host(`webdav.test`) && Method(`PROPFIND`)' \
    --label traefik.http.routers.nginx-webdav-propfind.entrypoints=web \
    --label traefik.http.routers.nginx-webdav-propfind.priority=100 \
    --label traefik.http.routers.nginx-webdav-propfind.middlewares=nginx-webdav-propfind-limit \
    --label traefik.http.routers.nginx-webdav-propfind.service=nginx-webdav \
    "$image" >/dev/null
docker run --detach --rm --name "$proxy" --network "$network" \
    --publish 127.0.0.1::80 --volume /var/run/docker.sock:/var/run/docker.sock:ro \
    "$traefik_image" --providers.docker=true --providers.docker.exposedbydefault=false \
    --entrypoints.web.address=:80 --log.level=ERROR >/dev/null
port=$(docker port "$proxy" 80/tcp | sed -E 's/.*:([0-9]+)$/\1/')

body=$(mktemp)
trap 'rm -f "$body"; cleanup' EXIT

printf '%s' '<?xml version="1.0"?><propfind xmlns="DAV:"><propname/></propfind>' >"$body"
for _ in {1..30}; do
    small_status=$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
        --header 'Host: webdav.test' --request PROPFIND --data-binary @"$body" \
        "http://127.0.0.1:$port/" || true)
    test "$small_status" = 207 && break
    sleep 0.2
done
test "$small_status" = 207

{
    printf '%s' '<?xml version="1.0"?><propfind xmlns="DAV:"><propname>'
    head -c 16384 /dev/zero | tr '\0' x
    printf '%s' '</propname></propfind>'
} >"$body"
large_status=$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
    --header 'Host: webdav.test' --request PROPFIND --data-binary @"$body" \
    "http://127.0.0.1:$port/")
test "$large_status" = 413

put_status=$(curl --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
    --header 'Host: webdav.test' --request PUT --data-binary @"$body" \
    "http://127.0.0.1:$port/large.txt")
test "$put_status" = 201

printf 'PROPFIND small body: %s; oversized body: %s; large PUT: %s\n' \
    "$small_status" "$large_status" "$put_status"
