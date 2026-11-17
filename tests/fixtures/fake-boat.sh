#!/bin/sh
# A fake `boat` CLI for the `kontext dispatch` tests (KONTEXT_BOAT points at
# a copy of it). It never talks to Boat. Its output follows what Boat's CLI
# 1.0.100 printed on 2026-11-16 (`boat exec` sync, --detach, --status and
# --kill). Everything it is asked goes in $FAKE_BOAT_DIR/calls.log, one line
# per call, so a test can check that no secret was ever in an argument.
# Files under $FAKE_BOAT_DIR steer it:
#
#   fail-new          `new` fails before making a sandbox
#   fail-new-late     `new` makes a sandbox, then fails
#   fail-stop         `stop` fails
#   fail-detach       `exec --detach` fails
#   fail-status       `exec --status` fails (the CLI could not ask)
#   fail-rm           the credential's `rm` fails in the sandbox
#   <id>.setup        `info` says this setupStatus (default: done)
#   <id>.setup-error  and this setupError
#   end-<id>-<n>      ends turn n on that sandbox. Its content says how:
#                       empty           Claude Code ended the turn (exit 0)
#                       error:<text>    Claude Code failed with <text> (exit 1)
#                       exit:<code>     the runner exited <code>, no result
#   lost-<id>-<n>     `--status` does not know turn n's process
#   hang-new          `new` makes the sandbox, then waits (until release-new
#                     exists, 60 s at most) before it says it is ready
#   hang-stop         `stop` waits the same way (until release-stop)
#
# A second `exec --detach` of a turn already started gets a process that
# exits 90 at once, as the runner does when it refuses to run a turn twice.
#   end-all           ends every turn as the first form does
#
# What it keeps: files/<name> for each --file (the file's base name),
# <id>.setup-file for --setup-file, <id>.env with the -e values, <id>.stopped
# once stopped, <id>.name, pid-<pid> ("<id> <n> <session>") per detached
# turn and killed-<pid> once killed. `list` shows the sandboxes not stopped.
set -eu
D=${FAKE_BOAT_DIR:?FAKE_BOAT_DIR is not set}
mkdir -p "$D/files"
printf '%s\n' "$*" >>"$D/calls.log"

cmd=${1:-}
[ $# -gt 0 ] && shift

# Waits for release-$1 while hang-$1 is there (60 s at most).
hang() {
    i=0
    while [ -e "$D/hang-$1" ] && [ ! -e "$D/release-$1" ] && [ "$i" -lt 600 ]; do
        i=$((i + 1))
        sleep 0.1
    done
}

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
        --name) name=$2; shift 2 ;;
        --type | --ttl | --environment) shift 2 ;;
        *) shift ;;
        esac
    done
    if [ -e "$D/fail-new" ]; then
        echo '{"event":"error","error":"no_ready_machine","code":"no_ready_machine"}'
        exit 1
    fi
    id=$(next_id)
    echo "{\"event\":\"created\",\"id\":\"$id\"}"
    printf '%s' "${name:-}" >"$D/$id.name"
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
    hang new
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
    hang stop
    if [ -e "$D/fail-stop" ]; then
        echo '{"event":"error","error":"stop failed","code":"x"}'
        exit 1
    fi
    : >"$D/$id.stopped"
    echo "{\"id\":\"$id\",\"state\":\"stopping\"}"
    ;;
list)
    printf '['
    sep=""
    for f in "$D"/bx_fake*.name; do
        [ -e "$f" ] || continue
        id=$(basename "$f" .name)
        [ -e "$D/$id.stopped" ] && continue
        printf '%s{"id":"%s","name":"%s","state":"ready"}' "$sep" "$id" "$(cat "$f")"
        sep=","
    done
    echo ']'
    ;;
exec)
    id=$1
    shift
    mode=sync
    pid=""
    while [ $# -gt 0 ]; do
        case "$1" in
        --detach) mode=detach; shift ;;
        --status) mode=status; pid=$2; shift 2 ;;
        --kill) mode=kill; pid=$2; shift 2 ;;
        --json) shift ;;
        --timeout | --cwd) shift 2 ;;
        --) shift; break ;;
        *) break ;;
        esac
    done
    line="$*"
    # How turn n (of sandbox $1) ended: sets `how` to ended, error:<text>,
    # exit:<code> or running.
    turn_end() {
        if [ -e "$D/end-$1-$2" ]; then
            how=$(cat "$D/end-$1-$2")
            [ -n "$how" ] || how=ended
        elif [ -e "$D/end-all" ]; then
            how=ended
        else
            how=running
        fi
    }
    case "$mode" in
    detach)
        if [ -e "$D/fail-detach" ]; then
            echo '{"event":"error","error":"exec failed","code":"x"}'
            exit 1
        fi
        n=$(printf '%s' "$line" | sed -n 's/.* start \([0-9][0-9]*\) \([0-9a-f-]*\)$/\1/p')
        sid=$(printf '%s' "$line" | sed -n 's/.* start \([0-9][0-9]*\) \([0-9a-f-]*\)$/\2/p')
        p=$(cat "$D/pids" 2>/dev/null || echo 7200)
        p=$((p + 1))
        echo "$p" >"$D/pids"
        if grep -q "^$id ${n:-0} " "$D"/pid-* 2>/dev/null; then
            : >"$D/dup-$p"
        fi
        echo "$id ${n:-0} ${sid:-none}" >"$D/pid-$p"
        echo "{\"command\":\"x\",\"commandId\":\"cmd_$p\",\"logPath\":\"/home/user/.ascii/processes/$p.log\",\"pid\":$p,\"processId\":$p,\"success\":true}"
        ;;
    status)
        if [ -e "$D/fail-status" ]; then
            echo "error: the Boat API did not answer" >&2
            exit 1
        fi
        if [ ! -e "$D/pid-$pid" ]; then
            echo "{\"known\":false,\"running\":false,\"status\":\"lost\",\"pid\":$pid}"
            exit 0
        fi
        read -r sid_ n _ <"$D/pid-$pid"
        if [ -e "$D/lost-$sid_-$n" ]; then
            echo "{\"known\":false,\"running\":false,\"status\":\"lost\",\"pid\":$pid}"
        elif [ -e "$D/dup-$pid" ]; then
            echo "{\"known\":true,\"running\":false,\"status\":\"exited\",\"exitCode\":90,\"signal\":null,\"pid\":$pid}"
        elif [ -e "$D/killed-$pid" ]; then
            echo "{\"known\":true,\"running\":false,\"status\":\"exited\",\"exitCode\":143,\"signal\":\"SIGTERM\",\"pid\":$pid}"
        else
            turn_end "$sid_" "$n"
            case "$how" in
            running) echo "{\"known\":true,\"running\":true,\"status\":\"running\",\"exitCode\":null,\"signal\":null,\"pid\":$pid}" ;;
            ended) echo "{\"known\":true,\"running\":false,\"status\":\"exited\",\"exitCode\":0,\"signal\":null,\"pid\":$pid}" ;;
            error:*) echo "{\"known\":true,\"running\":false,\"status\":\"exited\",\"exitCode\":1,\"signal\":null,\"pid\":$pid}" ;;
            exit:*) echo "{\"known\":true,\"running\":false,\"status\":\"exited\",\"exitCode\":${how#exit:},\"signal\":null,\"pid\":$pid}" ;;
            esac
        fi
        ;;
    kill)
        : >"$D/killed-$pid"
        echo "{\"running\":false,\"signaled\":true,\"command\":{\"status\":\"killed\",\"exitCode\":143,\"signal\":\"SIGTERM\"},\"pid\":$pid}"
        ;;
    sync)
        out=""
        code=0
        case "$line" in
        *" read "[0-9]*)
            n=${line##* read }
            p=$(grep -l "^$id $n " "$D"/pid-* 2>/dev/null | head -n 1 | sed 's/.*pid-//')
            turn_end "$id" "$n"
            [ -n "$p" ] && [ -e "$D/killed-$p" ] && how=killed
            case "$how" in
            ended) out="pid=$p\\nexit=0\\nflag=\\\"is_error\\\":false\\n--- result\\n{\\\"type\\\":\\\"result\\\",\\\"is_error\\\":false,\\\"result\\\":\\\"done\\\"}\\n--- stderr\\n\\n" ;;
            error:*)
                t=$(printf '%s' "${how#error:}" | sed 's/["\\]/ /g')
                out="pid=$p\\nexit=1\\nflag=\\\"is_error\\\":true\\n--- result\\n{\\\"type\\\":\\\"result\\\",\\\"is_error\\\":true,\\\"result\\\":\\\"$t\\\"}\\n--- stderr\\n\\n" ;;
            exit:*) out="pid=$p\\nexit=${how#exit:}\\nflag=\\n--- result\\n\\n--- stderr\\nkontext-dispatch: the runner failed\\n" ;;
            *) out="pid=$p\\nexit=\\nflag=\\n--- result\\n\\n--- stderr\\n\\n" ;;
            esac
            ;;
        *"rm -f"*)
            [ -e "$D/fail-rm" ] && code=1
            : >"$D/$id.credential-removed"
            ;;
        esac
        printf '%s\n' "{\"commandId\":\"cmd_s\",\"exitCode\":$code,\"stdout\":\"$out\",\"stderr\":\"\",\"stdoutTruncated\":false,\"stderrTruncated\":false,\"success\":true,\"timedOut\":false}"
        ;;
    esac
    ;;
*)
    echo "fake boat: $cmd is not faked" >&2
    exit 2
    ;;
esac
