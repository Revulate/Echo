#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: %s [--timeout-seconds N] [--heartbeat-seconds N] [--kill-after-seconds N] [--log PATH] -- NAME COMMAND [ARG ...]\n' "${0##*/}" >&2
}

timeout_seconds="${ECHO_LINUX_STAGE_TIMEOUT_SECONDS:-900}"
heartbeat_seconds="${ECHO_LINUX_STAGE_HEARTBEAT_SECONDS:-30}"
kill_after_seconds="${ECHO_LINUX_STAGE_KILL_AFTER_SECONDS:-10}"
log_path=""

while (($#)); do
    case "$1" in
        --timeout-seconds)
            timeout_seconds="${2:?missing timeout value}"
            shift 2
            ;;
        --heartbeat-seconds)
            heartbeat_seconds="${2:?missing heartbeat value}"
            shift 2
            ;;
        --kill-after-seconds)
            kill_after_seconds="${2:?missing kill-after value}"
            shift 2
            ;;
        --log)
            log_path="${2:?missing log path}"
            shift 2
            ;;
        --)
            shift
            break
            ;;
        *)
            usage
            exit 2
            ;;
    esac
done

if (($# < 2)); then
    usage
    exit 2
fi
if [[ ! "$timeout_seconds" =~ ^[1-9][0-9]*$ || ! "$heartbeat_seconds" =~ ^[1-9][0-9]*$ || ! "$kill_after_seconds" =~ ^[1-9][0-9]*$ ]]; then
    printf 'Stage timeout, heartbeat, and kill-after values must be positive integers.\n' >&2
    exit 2
fi

stage_name="$1"
shift
command=("$@")
if [[ -z "$log_path" ]]; then
    safe_name="${stage_name//[^[:alnum:]_.-]/-}"
    log_path="${ECHO_LINUX_STAGE_LOG_DIR:-target/linux-test-stages}/$safe_name.log"
fi
mkdir -p "$(dirname -- "$log_path")"
: >"$log_path"

start_epoch="$(date +%s)"
printf 'START stage=%s timeout=%ss log=%s command=' "$stage_name" "$timeout_seconds" "$log_path"
printf '%q ' "${command[@]}"
printf '\n'

setsid -- "${command[@]}" >"$log_path" 2>&1 &
stage_pid=$!

process_group_alive() {
    kill -0 -- "-$stage_pid" 2>/dev/null
}

terminate_process_group() {
    local deadline=$(( $(date +%s) + kill_after_seconds ))
    kill -TERM -- "-$stage_pid" 2>/dev/null || true
    while process_group_alive && (( $(date +%s) < deadline )); do
        sleep 1
    done
    if process_group_alive; then
        kill -KILL -- "-$stage_pid" 2>/dev/null || true
    fi
}

interrupt_stage() {
    local signal_name="$1"
    local exit_status="$2"
    trap - INT TERM HUP
    printf 'FAIL stage=%s signal=%s action=TERM log=%s\n' "$stage_name" "$signal_name" "$log_path" >&2
    terminate_process_group
    wait "$stage_pid" 2>/dev/null || true
    exit "$exit_status"
}

trap 'interrupt_stage INT 130' INT
trap 'interrupt_stage TERM 143' TERM
trap 'interrupt_stage HUP 129' HUP
deadline=$((start_epoch + timeout_seconds))
next_heartbeat=$((start_epoch + heartbeat_seconds))
timed_out=0

while kill -0 "$stage_pid" 2>/dev/null; do
    now="$(date +%s)"
    if ((now >= deadline)); then
        timed_out=1
        printf 'TIMEOUT stage=%s elapsed=%ss action=TERM\n' "$stage_name" "$((now - start_epoch))" >&2
        terminate_process_group
        if process_group_alive; then
            printf 'TIMEOUT stage=%s elapsed=%ss action=KILL\n' "$stage_name" "$(( $(date +%s) - start_epoch))" >&2
        fi
        break
    fi
    if ((now >= next_heartbeat)); then
        printf 'HEARTBEAT stage=%s elapsed=%ss log=%s\n' "$stage_name" "$((now - start_epoch))" "$log_path"
        ps -o pid=,ppid=,stat=,etime=,comm= --sid "$stage_pid" 2>/dev/null \
            | sed -n '1,12p' \
            | sed 's/^/HEARTBEAT process=/' || true
        next_heartbeat=$((now + heartbeat_seconds))
    fi
    sleep 1
done

set +e
wait "$stage_pid"
status=$?
set -e
elapsed=$(( $(date +%s) - start_epoch ))

if ((timed_out == 0)) && process_group_alive; then
    printf 'FAIL stage=%s status=descendant-leak elapsed=%ss action=TERM\n' "$stage_name" "$elapsed" >&2
    terminate_process_group
    status=125
fi

if ((timed_out)); then
    printf 'TIMEOUT stage=%s elapsed=%ss log=%s\n' "$stage_name" "$elapsed" "$log_path" >&2
    printf '%s\n' "----- retained stage log: $log_path -----" >&2
    tail -n 400 "$log_path" >&2
    exit 124
fi
if ((status != 0)); then
    printf 'FAIL stage=%s status=%d elapsed=%ss log=%s\n' "$stage_name" "$status" "$elapsed" "$log_path" >&2
    printf '%s\n' "----- retained stage log: $log_path -----" >&2
    tail -n 400 "$log_path" >&2
    exit "$status"
fi

printf 'PASS stage=%s elapsed=%ss log=%s\n' "$stage_name" "$elapsed" "$log_path"
