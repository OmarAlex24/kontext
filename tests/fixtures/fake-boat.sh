#!/bin/sh
# A fake `boat` CLI for the `kontext dispatch` tests (KONTEXT_BOAT points at
# a copy of it). It never talks to Boat. Everything it is asked goes in
# $FAKE_BOAT_DIR/calls.log, one line per call, so a test can check that no
# secret was ever in an argument. Files under $FAKE_BOAT_DIR steer it:
#
#   fail-new          `new` fails before making a sandbox
#   fail-new-late     `new` makes a sandbox, then fails
#   fail-stop         `stop` fails
#   <id>.setup        `info` says this setupStatus (default: done)
#   <id>.setup-error  and this setupError
#
# What it keeps: files/<name> for each --file (the file's base name),
# <id>.setup-file for --setup-file, <id>.env with the -e values, and
# <id>.stopped once stopped.
set -eu
D=${FAKE_BOAT_DIR:?FAKE_BOAT_DIR is not set}
mkdir -p "$D/files"
printf '%s\n' "$*" >>"$D/calls.log"

cmd=${1:-}
[ $# -gt 0 ] && shift

next_id() {
    n=$(cat "$D/counter" 2>/dev/null || echo 0)
    n=$((n + 1))
    echo "$n" >"$D/counter"
    echo "bx_fake$n"
}

case "$cmd" in
new)
    envs=""
    files=""
    setup=""
    while [ $# -gt 0 ]; do
        case "$1" in
        -e | --env) envs="$envs$2
"; shift 2 ;;
        --file) files="$files$2
"; shift 2 ;;
        --setup-file) setup=$2; shift 2 ;;
        --name | --type | --ttl | --environment) shift 2 ;;
        *) shift ;;
        esac
    done
    if [ -e "$D/fail-new" ]; then
        echo '{"event":"error","error":"no_ready_machine","code":"no_ready_machine"}'
        exit 1
    fi
    id=$(next_id)
    echo "{\"event\":\"created\",\"id\":\"$id\"}"
    printf '%s' "$envs" >"$D/$id.env"
    printf '%s' "$files" | while IFS= read -r f; do
        [ -n "$f" ] || continue
        there=${f%%=*}
        here=${f#*=}
        cp "$here" "$D/files/$(basename "$there")"
    done
    if [ -n "$setup" ]; then cp "$setup" "$D/$id.setup-file"; fi
    if [ -e "$D/fail-new-late" ]; then
        echo '{"event":"error","error":"the machine went away","code":"lost"}'
        exit 1
    fi
    echo '{"event":"state","state":"starting"}'
    echo "{\"event\":\"ready\",\"id\":\"$id\",\"state\":\"running\"}"
    ;;
info)
    id=$1
    status=$(cat "$D/$id.setup" 2>/dev/null || echo done)
    err=$(cat "$D/$id.setup-error" 2>/dev/null || echo "")
    echo "{\"id\":\"$id\",\"state\":\"running\",\"setupStatus\":\"$status\",\"setupError\":\"$err\"}"
    ;;
stop)
    id=$1
    if [ -e "$D/fail-stop" ]; then
        echo '{"event":"error","error":"stop failed","code":"x"}'
        exit 1
    fi
    : >"$D/$id.stopped"
    echo "{\"id\":\"$id\",\"state\":\"stopping\"}"
    ;;
*)
    echo "fake boat: $cmd is not faked" >&2
    exit 2
    ;;
esac
