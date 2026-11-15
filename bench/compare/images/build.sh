#!/bin/sh
# Builds every image of the comparison benchmark with the versions in
# versions.env and prints a JSON line per image with its id (digest). run.py
# records these in every run.json.
#
#   bench/compare/images/build.sh [image ...]   # default: all
#
# Images: base plain kontext gentle-ai mem0 beads openspec sidecar verify.
# No key reaches a build: none is passed as a build argument or environment.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
# shellcheck disable=SC1091
. "$here/versions.env"

all="base plain kontext gentle-ai mem0 beads openspec sidecar verify"
want=${*:-$all}

build() {
    name=$1; shift
    docker build --quiet --tag "bench-$name:latest" "$@" >/dev/null
    id=$(docker image inspect "bench-$name:latest" --format '{{.Id}}')
    printf '{"image": "bench-%s:latest", "id": "%s"}\n' "$name" "$id"
}

for n in $want; do
    case "$n" in
        base)
            build base --build-arg NODE_IMAGE="$NODE_IMAGE" \
                --build-arg CLAUDE_CODE_VERSION="$CLAUDE_CODE_VERSION" \
                -f "$here/Dockerfile.base" "$here" ;;
        plain|kontext|gentle-ai|mem0|beads|openspec)
            build "$n" --build-arg BASE=bench-base:latest \
                --build-arg KONTEXT_VERSION="$KONTEXT_VERSION" \
                --build-arg GENTLE_AI_VERSION="$GENTLE_AI_VERSION" \
                --build-arg MEM0_REF="$MEM0_REF" \
                --build-arg BEADS_VERSION="$BEADS_VERSION" \
                --build-arg DOLT_VERSION="$DOLT_VERSION" \
                --build-arg OPENSPEC_VERSION="$OPENSPEC_VERSION" \
                -f "$here/Dockerfile.$n" "$here" ;;
        sidecar)
            ctx=$(mktemp -d)
            trap 'rm -rf "$ctx"' EXIT
            git -C "$repo" archive "$KONTEXT_VERSION" server | tar -x -C "$ctx"
            cp "$here/sidecar.mjs" "$ctx/"
            build sidecar --build-arg NODE_IMAGE="$NODE_IMAGE" -f "$here/Dockerfile.sidecar" "$ctx" ;;
        verify)
            build verify --build-arg NODE_IMAGE="$NODE_IMAGE" -f "$here/Dockerfile.verify" "$here" ;;
        *) echo "build.sh: unknown image $n" >&2; exit 2 ;;
    esac
done
