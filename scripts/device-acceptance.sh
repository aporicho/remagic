#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}
RUNTIME_UNIT=${REMAGIC_RUNTIME_UNIT:-remagic-runtime.service}
CONTROL_SOCKET=${REMAGIC_RUNTIME_SOCKET:-/run/remagic/runtime-app.sock}
QTFB_SOCKET=${REMAGIC_QTFB_SOCKET:-/tmp/qtfb.sock}
TAP_TOOL=${REMAGIC_TAP_TOOL:-/home/root/apps/remagic/bin/remagic-uinput-tap}
TAP_FIRST_CARD_X=${REMAGIC_TAP_FIRST_CARD_X:-137}
TAP_FIRST_CARD_Y=${REMAGIC_TAP_FIRST_CARD_Y:-220}
TAP_SECOND_CARD_X=${REMAGIC_TAP_SECOND_CARD_X:-357}
TAP_SECOND_CARD_Y=${REMAGIC_TAP_SECOND_CARD_Y:-220}
TAP_FIRST_CLOSE_X=${REMAGIC_TAP_FIRST_CLOSE_X:-199}
TAP_SECOND_CLOSE_X=${REMAGIC_TAP_SECOND_CLOSE_X:-419}
TAP_CLOSE_Y=${REMAGIC_TAP_CLOSE_Y:-153}
TAP_SYSTEM_X=${REMAGIC_TAP_SYSTEM_X:-145}
TAP_SYSTEM_Y=${REMAGIC_TAP_SYSTEM_Y:-52}
KOREADER_LOG=${REMAGIC_KOREADER_LOG:-/home/root/apps/koreader/crash.log}
# Stay in the global menu's extended top-center activation zone while avoiding
# both the file-manager path bar and the oversized top-right plus-button target.
KOREADER_MENU_X=${REMAGIC_KOREADER_MENU_X:-650}
KOREADER_MENU_Y=${REMAGIC_KOREADER_MENU_Y:-130}
KOREADER_EXIT_MENU_X=${REMAGIC_KOREADER_EXIT_MENU_X:-477}
KOREADER_EXIT_MENU_Y=${REMAGIC_KOREADER_EXIT_MENU_Y:-705}
KOREADER_EXIT_X=${REMAGIC_KOREADER_EXIT_X:-477}
KOREADER_EXIT_Y=${REMAGIC_KOREADER_EXIT_Y:-371}
KOREADER_PLUS_X=${REMAGIC_KOREADER_PLUS_X:-800}
KOREADER_PLUS_Y=${REMAGIC_KOREADER_PLUS_Y:-30}
KOREADER_NEW_FOLDER_X=${REMAGIC_KOREADER_NEW_FOLDER_X:-477}
KOREADER_NEW_FOLDER_Y=${REMAGIC_KOREADER_NEW_FOLDER_Y:-705}
QTFB_WIDTH=${REMAGIC_QTFB_WIDTH:-954}
QTFB_HEIGHT=${REMAGIC_QTFB_HEIGHT:-1696}
TEST_STARTED_AT=$(date +%s)
FRAME_EVIDENCE_DIR=${REMAGIC_FRAME_EVIDENCE_DIR:-/tmp/remagic-acceptance-frames-$TEST_STARTED_AT}
RUNTIME_EVENT_WAIT_POLLS=${REMAGIC_RUNTIME_EVENT_WAIT_POLLS:-300}
MAGICPAPER_AGENT_WAS_ACTIVE=false
if [ "$(systemctl is-active magicpaper-agent.service 2>/dev/null || true)" = active ]; then
    MAGICPAPER_AGENT_WAS_ACTIVE=true
fi

checkpoint() {
    cursor=$(journalctl -n 0 --show-cursor --no-pager 2>/dev/null \
        | sed -n 's/^-- cursor: //p' | tail -n 1)
    if [ -n "$cursor" ]; then
        printf 'cursor:%s\n' "$cursor"
    else
        printf 'time:%s\n' "$(date +%s)"
    fi
}

TEST_CHECKPOINT=$(checkpoint)

journal_after() (
    point=$1
    shift
    case "$point" in
        cursor:*)
            journalctl --after-cursor="${point#cursor:}" "$@" --no-pager 2>/dev/null \
                || journalctl --since="@$TEST_STARTED_AT" "$@" --no-pager 2>/dev/null \
                || true
            ;;
        time:*)
            journalctl --since="@${point#time:}" "$@" --no-pager 2>/dev/null || true
            ;;
    esac
)

runtime_log_after() {
    journal_after "$1" -u "$RUNTIME_UNIT" -o cat
}

section() {
    printf '\n[acceptance] %s\n' "$1"
}

dump_diagnostics() {
    printf '\n[acceptance] FAILURE DIAGNOSTICS\n' >&2
    printf '[acceptance] remagic status\n' >&2
    "$CTL" status >&2 || true
    printf '[acceptance] application catalog\n' >&2
    "$CTL" apps >&2 || true
    printf '[acceptance] relevant unit state\n' >&2
    systemctl status --no-pager remagicd.service "$RUNTIME_UNIT" \
        magicpaper-agent.service xochitl.service paperweight.service >&2 || true
    printf '[acceptance] relevant processes\n' >&2
    ps -eo pid,ppid,stat,comm,args >&2 2>/dev/null || ps w >&2 || true
    printf '[acceptance] journal from this test\n' >&2
    journal_after "$TEST_CHECKPOINT" \
        -u remagicd.service -u "$RUNTIME_UNIT" -u magicpaper-agent.service \
        -u xochitl.service -u paperweight.service -o short-monotonic \
        | tail -n 600 >&2 || true
    if [ -f "$KOREADER_LOG" ]; then
        printf '[acceptance] KOReader crash.log tail\n' >&2
        tail -n 300 "$KOREADER_LOG" >&2 || true
    fi
    for framebuffer in /dev/shm/qtfb_*; do
        [ -f "$framebuffer" ] || continue
        snapshot="/tmp/remagic-acceptance-failure-$(basename "$framebuffer")"
        cp "$framebuffer" "$snapshot" 2>/dev/null || true
        printf '[acceptance] active QTFB snapshot: %s\n' "$snapshot" >&2
    done
    if command -v coredumpctl >/dev/null 2>&1; then
        printf '[acceptance] core dumps from this test\n' >&2
        coredumpctl --since="@$TEST_STARTED_AT" --no-pager list >&2 || true
    fi
}

cleanup() {
    status=$?
    trap - 0 1 2 15
    if [ "$status" -ne 0 ]; then
        dump_diagnostics
        # A failed test must not strand the tablet in the alternative display
        # domain. Preserve diagnostics first, then request the serialized safe
        # return path without hiding the original failure status.
        if ! "$CTL" system >/dev/null 2>&1; then
            /home/root/apps/remagic/libexec/remagic-recover >/dev/null 2>&1 || true
        fi
    fi
    if [ "$MAGICPAPER_AGENT_WAS_ACTIVE" = true ]; then
        systemctl start magicpaper-agent.service >/dev/null 2>&1 || true
    fi
    exit "$status"
}

trap cleanup 0
trap 'exit 129' 1
trap 'exit 130' 2
trap 'exit 143' 15

fail() {
    printf '[acceptance] ERROR: %s\n' "$1" >&2
    return 1
}

wait_state() (
    wanted=$1
    attempts=0
    while [ "$attempts" -lt 120 ]; do
        state=$("$CTL" status 2>/dev/null || true)
        case "$wanted" in
            manager)
                printf '%s' "$state" | grep -q '"domain": "manager"' && return 0
                ;;
            system)
                printf '%s' "$state" | grep -q '"domain": "system"' && return 0
                ;;
            foreground:*)
                app=${wanted#foreground:}
                printf '%s' "$state" | grep -q "\"foreground\": \"$app\"" && return 0
                ;;
        esac
        sleep 0.25
        attempts=$((attempts + 1))
    done
    printf '[acceptance] timeout waiting for state %s\n' "$wanted" >&2
    "$CTL" status >&2 || true
    return 1
)

wait_service() (
    unit=$1
    wanted=$2
    attempts=0
    while [ "$attempts" -lt 120 ]; do
        [ "$(systemctl is-active "$unit" 2>/dev/null || true)" = "$wanted" ] && return 0
        sleep 0.25
        attempts=$((attempts + 1))
    done
    printf '[acceptance] timeout waiting for %s=%s (actual=%s)\n' \
        "$unit" "$wanted" "$(systemctl is-active "$unit" 2>/dev/null || true)" >&2
    return 1
)

unit_loaded() {
    [ "$(systemctl show --property=LoadState --value "$1" 2>/dev/null || true)" = loaded ]
}

wait_path() (
    path=$1
    wanted=$2
    attempts=0
    while [ "$attempts" -lt 120 ]; do
        if [ "$wanted" = present ] && [ -S "$path" ]; then
            return 0
        fi
        if [ "$wanted" = absent ] && [ ! -e "$path" ]; then
            return 0
        fi
        sleep 0.25
        attempts=$((attempts + 1))
    done
    fail "timeout waiting for $path to become $wanted"
)

runtime_event_present() (
    point=$1
    app=$2
    event=$3
    runtime_log_after "$point" | awk \
        -v app_token="app=$app" -v event_token="event=$event" '
        {
            has_app = 0
            has_event = 0
            for (field = 1; field <= NF; field++) {
                if ($field == app_token) has_app = 1
                if ($field == event_token) has_event = 1
            }
            if (has_app && has_event) found = 1
        }
        END { exit(found ? 0 : 1) }
    '
)

wait_runtime_event() (
    point=$1
    app=$2
    event=$3
    max_attempts=${4:-$RUNTIME_EVENT_WAIT_POLLS}
    attempts=0
    while [ "$attempts" -lt "$max_attempts" ]; do
        if runtime_event_present "$point" "$app" "$event"; then
            return 0
        fi
        sleep 0.1
        attempts=$((attempts + 1))
    done
    printf '[acceptance] missing runtime event after %s polls: app=%s event=%s\n' \
        "$max_attempts" "$app" "$event" >&2
    printf '[acceptance] runtime state at event timeout\n' >&2
    "$CTL" status >&2 || true
    runtime_log_after "$point" | tail -n 160 >&2 || true
    return 1
)

wait_ui_app_pressed() (
    point=$1
    attempts=0
    while [ "$attempts" -lt 160 ]; do
        app=$(runtime_log_after "$point" \
            | sed -n 's/.*ui=app-card app=\([^ ]*\) event=pressed.*/\1/p' \
            | sed -n '1p')
        if [ -n "$app" ]; then
            case "$app" in
                magicpaper|koreader)
                    printf '%s\n' "$app"
                    return 0
                    ;;
                *)
                    printf '[acceptance] touch selected unknown app id: %s\n' "$app" >&2
                    return 1
                    ;;
            esac
        fi
        sleep 0.1
        attempts=$((attempts + 1))
    done
    printf '[acceptance] missing runtime UI event: ui=app-card event=pressed\n' >&2
    runtime_log_after "$point" | tail -n 160 >&2 || true
    return 1
)

wait_ui_system_pressed() (
    point=$1
    attempts=0
    while [ "$attempts" -lt 160 ]; do
        if runtime_log_after "$point" | grep -Fq 'ui=system-button event=pressed'; then
            return 0
        fi
        sleep 0.1
        attempts=$((attempts + 1))
    done
    printf '[acceptance] missing runtime UI event: ui=system-button event=pressed\n' >&2
    runtime_log_after "$point" | tail -n 160 >&2 || true
    return 1
)

wait_ui_close_pressed() (
    point=$1
    app=$2
    attempts=0
    while [ "$attempts" -lt 160 ]; do
        if runtime_log_after "$point" | grep -Fq "ui=close-button app=$app event=pressed"; then
            return 0
        fi
        sleep 0.1
        attempts=$((attempts + 1))
    done
    printf '[acceptance] missing close-button press for %s\n' "$app" >&2
    runtime_log_after "$point" | tail -n 160 >&2 || true
    return 1
)

wait_runtime_pattern() (
    point=$1
    pattern=$2
    description=$3
    attempts=${4:-300}
    count=0
    while [ "$count" -lt "$attempts" ]; do
        if runtime_log_after "$point" | grep -Eq "$pattern"; then
            return 0
        fi
        sleep 0.1
        count=$((count + 1))
    done
    printf '[acceptance] missing runtime pattern (%s): %s\n' "$description" "$pattern" >&2
    runtime_log_after "$point" | tail -n 200 >&2 || true
    return 1
)

assert_no_runtime_event() (
    point=$1
    app=$2
    event=$3
    if runtime_event_present "$point" "$app" "$event"; then
        fail "unexpected runtime event app=$app event=$event"
    fi
)

runtime_event_count() (
    point=$1
    app=$2
    event=$3
    runtime_log_after "$point" | awk -v app_token="app=$app" -v event_token="event=$event" '
        {
            has_app = 0
            has_event = 0
            for (field = 1; field <= NF; field++) {
                if ($field == app_token) has_app = 1
                if ($field == event_token) has_event = 1
            }
            if (has_app && has_event) count++
        }
        END { print count + 0 }
    '
)

assert_runtime_event_count() (
    point=$1
    app=$2
    event=$3
    expected=$4
    actual=$(runtime_event_count "$point" "$app" "$event")
    [ "$actual" = "$expected" ] \
        || fail "expected app=$app event=$event exactly $expected time(s), found $actual"
)

assert_no_black_white_refresh_sequence() (
    point=$1
    app=$2
    offending=$(runtime_log_after "$point" | awk -v app_token="app=$app" '
        {
            has_app = 0
            is_refresh = 0
            has_black_or_white_phase = 0
            for (field = 1; field <= NF; field++) {
                token = $field
                if (token == app_token) has_app = 1
                if (token ~ /^event=.*refresh/ || token == "display=refresh") is_refresh = 1
                if (token ~ /^(phase|color|sequence|strategy|mode)=(black|white|black-white|white-black)$/ ||
                    token == "event=black-refresh" || token == "event=white-refresh" ||
                    token == "event=refresh-black" || token == "event=refresh-white" ||
                    token == "event=full-clean-refresh-black" ||
                    token == "event=full-clean-refresh-white") {
                    has_black_or_white_phase = 1
                }
            }
            if (has_app && is_refresh && has_black_or_white_phase) print
        }
    ')
    if [ -n "$offending" ]; then
        printf '[acceptance] forbidden black/white refresh sequence for %s:\n%s\n' \
            "$app" "$offending" >&2
        return 1
    fi
)

verify_single_clean_refresh() (
    app=$1
    point=$2
    wait_runtime_event "$point" "$app" first-frame
    wait_runtime_event "$point" "$app" foreground
    wait_runtime_event "$point" "$app" full-clean-refresh-complete
    assert_runtime_event_order "$point" "$app" first-frame "$app" foreground
    assert_runtime_event_order "$point" "$app" foreground "$app" full-clean-refresh-complete
    # A legacy black-then-white cleanup emits its second phase shortly after
    # the first. Give the runtime time to expose a duplicate before counting.
    sleep 1
    assert_runtime_event_count "$point" "$app" full-clean-refresh-complete 1
    assert_no_black_white_refresh_sequence "$point" "$app"
)

runtime_event_line() (
    point=$1
    app=$2
    event=$3
    runtime_log_after "$point" | awk \
        -v app_token="app=$app" -v event_token="event=$event" '
        {
            has_app = 0
            has_event = 0
            for (field = 1; field <= NF; field++) {
                if ($field == app_token) has_app = 1
                if ($field == event_token) has_event = 1
            }
            if (has_app && has_event) {
                print NR
                exit
            }
        }
    '
)

assert_runtime_event_order() (
    point=$1
    first_app=$2
    first_event=$3
    second_app=$4
    second_event=$5
    first_line=$(runtime_event_line "$point" "$first_app" "$first_event")
    second_line=$(runtime_event_line "$point" "$second_app" "$second_event")
    [ -n "$first_line" ] && [ -n "$second_line" ] \
        && [ "$first_line" -lt "$second_line" ] \
        || fail "runtime event order is invalid: app=$first_app event=$first_event must precede app=$second_app event=$second_event"
)

runtime_pattern_line() (
    point=$1
    pattern=$2
    runtime_log_after "$point" \
        | awk -v pattern="$pattern" '$0 ~ pattern { print NR; exit }'
)

assert_runtime_pattern_order() (
    point=$1
    first_pattern=$2
    first_description=$3
    second_pattern=$4
    second_description=$5
    first_line=$(runtime_pattern_line "$point" "$first_pattern")
    second_line=$(runtime_pattern_line "$point" "$second_pattern")
    [ -n "$first_line" ] && [ -n "$second_line" ] \
        && [ "$first_line" -lt "$second_line" ] \
        || fail "runtime order is invalid: $first_description must precede $second_description"
)

real_app_pids() (
    app=$1
    for proc in /proc/[0-9]*; do
        [ -r "$proc/cmdline" ] || continue
        command_line=$(tr '\000' ' ' < "$proc/cmdline" 2>/dev/null || true)
        case "$app:$command_line" in
            magicpaper:*\/riddle-qtfb* | koreader:*\/reader.lua*)
                # KOReader may fork a helper with the same reader.lua
                # command line. AppLoad intentionally puts the wrapper and
                # every descendant in one process group, which is the actual
                # singleton/lifecycle boundary.
                awk '{ print $5 }' "$proc/stat" 2>/dev/null
                ;;
        esac
    done | sort -nu
)

managed_app_pids() (
    app=$1
    for proc in /proc/[0-9]*; do
        [ -r "$proc/cmdline" ] || continue
        command_line=$(tr '\000' ' ' < "$proc/cmdline" 2>/dev/null || true)
        case "$app:$command_line" in
            magicpaper:*\/magicpaper-qtfb* \
                | magicpaper:*\/riddle-qtfb* \
                | magicpaper:*\/magicpaper-agent-remagic* \
                | magicpaper:*\/home\/root\/apps\/riddle\/* \
                | magicpaper:*\/home\/root\/apps\/remagic\/opt\/magicpaper\/*)
                printf '%s\n' "${proc#/proc/}"
                ;;
            koreader:*\/koreader-remagic* \
                | koreader:*\/reader.lua* \
                | koreader:*\/home\/root\/apps\/koreader\/* \
                | koreader:*\/home\/root\/apps\/remagic-koreader\/*)
                printf '%s\n' "${proc#/proc/}"
                ;;
        esac
    done
)

process_count() (
    pids=$(real_app_pids "$1")
    if [ -z "$pids" ]; then
        printf '0\n'
    else
        printf '%s\n' "$pids" | wc -l | tr -d '[:space:]'
        printf '\n'
    fi
)

managed_process_count() (
    pids=$(managed_app_pids "$1")
    if [ -z "$pids" ]; then
        printf '0\n'
    else
        printf '%s\n' "$pids" | wc -l | tr -d '[:space:]'
        printf '\n'
    fi
)

wait_process_count() (
    app=$1
    wanted=$2
    attempts=0
    while [ "$attempts" -lt 160 ]; do
        actual=$(process_count "$app")
        [ "$actual" = "$wanted" ] && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    printf '[acceptance] timeout waiting for %s real-process count=%s (actual=%s; pids=%s)\n' \
        "$app" "$wanted" "$(process_count "$app")" "$(real_app_pids "$app" | tr '\n' ' ')" >&2
    return 1
)

wait_managed_process_count() (
    app=$1
    wanted=$2
    attempts=0
    while [ "$attempts" -lt 160 ]; do
        actual=$(managed_process_count "$app")
        [ "$actual" = "$wanted" ] && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    printf '[acceptance] timeout waiting for %s managed-process count=%s (actual=%s; pids=%s)\n' \
        "$app" "$wanted" "$(managed_process_count "$app")" \
        "$(managed_app_pids "$app" | tr '\n' ' ')" >&2
    return 1
)

single_real_pid() (
    app=$1
    wait_process_count "$app" 1
    real_app_pids "$app" | sed -n '1p'
)

assert_same_single_process() (
    app=$1
    expected=$2
    wait_process_count "$app" 1
    actual=$(real_app_pids "$app" | sed -n '1p')
    [ "$actual" = "$expected" ] \
        || fail "$app process changed: expected PID $expected, found $actual"
)

runtime_pids() (
    for proc in /proc/[0-9]*; do
        [ -r "$proc/cmdline" ] || continue
        command_line=$(tr '\000' ' ' < "$proc/cmdline" 2>/dev/null || true)
        case "$command_line" in
            *\/remagic-appload-runtime*) printf '%s\n' "${proc#/proc/}" ;;
        esac
    done
)

runtime_process_count() (
    pids=$(runtime_pids)
    if [ -z "$pids" ]; then
        printf '0\n'
    else
        printf '%s\n' "$pids" | wc -l | tr -d '[:space:]'
        printf '\n'
    fi
)

assert_single_runtime() (
    [ "$(runtime_process_count)" = 1 ] \
        || fail "expected exactly one remagic-appload-runtime process"
)

single_runtime_pid() (
    assert_single_runtime
    runtime_pids | sed -n '1p'
)

assert_original_runtime() (
    assert_single_runtime
    actual=$(runtime_pids | sed -n '1p')
    [ "$actual" = "$RUNTIME_PID" ] \
        || fail "runtime process changed: expected PID $RUNTIME_PID, found $actual"
)

assert_no_legacy_hosts() (
    if unit_loaded remagic-home.service \
        && [ "$(systemctl is-active remagic-home.service 2>/dev/null || true)" = active ]; then
        fail "legacy remagic-home.service is active"
    fi
    old_units=$(systemctl list-units 'remagic-app@*.service' --state=active \
        --no-legend --no-pager 2>/dev/null || true)
    [ -z "$old_units" ] || fail "legacy remagic-app@ service is active: $old_units"
    for proc in /proc/[0-9]*; do
        [ -r "$proc/cmdline" ] || continue
        command_line=$(tr '\000' ' ' < "$proc/cmdline" 2>/dev/null || true)
        case "$command_line" in
            *\/remagic-home* | *\/remagic-runner*)
                fail "legacy host process survived: pid=${proc#/proc/} cmd=$command_line"
                ;;
        esac
    done
)

unit_activation_generation() (
    systemctl show --property=InactiveExitTimestampMonotonic --value "$1" \
        2>/dev/null || true
)

assert_managed_display_ownership() (
    xochitl_state=$(systemctl is-active xochitl.service 2>/dev/null || true)
    [ "$xochitl_state" = inactive ] \
        || fail "xochitl.service reclaimed the display in the managed domain (state=$xochitl_state)"
    [ "$(unit_activation_generation xochitl.service)" = "$XOCHITL_GENERATION" ] \
        || fail 'xochitl.service was activated during the managed-domain test'

    if unit_loaded paperweight.service; then
        paperweight_state=$(systemctl is-active paperweight.service 2>/dev/null || true)
        [ "$paperweight_state" = inactive ] \
            || fail "paperweight.service reclaimed the display in the managed domain (state=$paperweight_state)"
        [ "$(unit_activation_generation paperweight.service)" = "$PAPERWEIGHT_GENERATION" ] \
            || fail 'paperweight.service was activated during the managed-domain test'
    fi
)

assert_koreader_semantic_ready() (
    point=$1
    wait_runtime_event "$point" koreader semantic-ready

    process_line=$(runtime_log_after "$point" \
        | grep -F 'app=koreader event=process-started' \
        | sed -n '1p')
    process_pid=$(printf '%s\n' "$process_line" \
        | sed -n 's/.* pid=\([0-9][0-9]*\) generation=.*/\1/p')
    generation=$(printf '%s\n' "$process_line" \
        | sed -n 's/.* generation=\([0-9][0-9]*\).*/\1/p')
    [ -n "$process_pid" ] && [ -n "$generation" ] \
        || fail "KOReader process identity is missing from runtime log: $process_line"

    actual_pid=$(single_real_pid koreader)
    [ "$actual_pid" = "$process_pid" ] \
        || fail "KOReader semantic identity PID $process_pid does not match process group $actual_pid"

    wait_runtime_pattern "$point" \
        "remagic-koreader: event=patch-active pid=$process_pid generation=$generation" \
        'KOReader lifecycle patch activation'
    wait_runtime_pattern "$point" \
        "remagic-koreader: event=semantic-ready pid=$process_pid generation=$generation ui=(filemanager|reader)" \
        'KOReader main UI semantic readiness'

    [ -f /run/remagic/koreader-ready ] \
        || fail 'KOReader semantic-ready marker is absent'
    marker_lines=$(wc -l < /run/remagic/koreader-ready | tr -d '[:space:]')
    actual_identity=$(cat /run/remagic/koreader-ready)
    expected_identity=$(printf 'pid=%s\ngeneration=%s\n' "$process_pid" "$generation")
    [ "$marker_lines" = 2 ] && [ "$actual_identity" = "$expected_identity" ] \
        || fail "KOReader semantic-ready marker has the wrong identity: $actual_identity"

    assert_runtime_pattern_order "$point" \
        'app=koreader event=process-started' 'KOReader process start' \
        'remagic-koreader: event=patch-active' 'KOReader lifecycle patch activation'
    assert_runtime_pattern_order "$point" \
        'remagic-koreader: event=patch-active' 'KOReader lifecycle patch activation' \
        'remagic-koreader: event=semantic-ready' 'KOReader main UI readiness'
    assert_runtime_pattern_order "$point" \
        'remagic-koreader: event=semantic-ready' 'KOReader main UI readiness' \
        'app=koreader event=semantic-ready' 'runtime semantic-ready acknowledgement'
)

verify_new_launch() (
    app=$1
    point=$2
    wait_state "foreground:$app"
    wait_runtime_event "$point" "$app" starting
    wait_runtime_event "$point" "$app" qtfb-connected
    if [ "$app" = koreader ]; then
        assert_koreader_semantic_ready "$point"
    fi
    wait_runtime_event "$point" "$app" first-frame
    wait_runtime_event "$point" "$app" foreground
    assert_runtime_event_order "$point" "$app" starting "$app" qtfb-connected
    assert_runtime_event_order "$point" "$app" qtfb-connected "$app" first-frame
    if [ "$app" = koreader ]; then
        assert_runtime_event_order "$point" koreader qtfb-connected koreader semantic-ready
        assert_runtime_event_order "$point" koreader semantic-ready koreader first-frame
    fi
    verify_single_clean_refresh "$app" "$point"
    wait_process_count "$app" 1
    assert_original_runtime
    assert_managed_display_ownership
)

verify_duplicate_launch() (
    app=$1
    expected_pid=$2
    duplicate_point=$(checkpoint)
    "$CTL" launch "$app" >/dev/null
    sleep 1
    wait_state "foreground:$app"
    assert_same_single_process "$app" "$expected_pid"
    assert_no_runtime_event "$duplicate_point" "$app" starting
    assert_runtime_event_count "$duplicate_point" "$app" full-clean-refresh-complete 0
    assert_no_black_white_refresh_sequence "$duplicate_point" "$app"
    assert_original_runtime
    assert_managed_display_ownership
)

park_and_verify() (
    app=$1
    expected_pid=$2
    park_point=$(checkpoint)
    "$CTL" park >/dev/null
    wait_state manager
    wait_runtime_event "$park_point" "$app" background
    assert_same_single_process "$app" "$expected_pid"
    wait_service "$RUNTIME_UNIT" active
    assert_original_runtime
    assert_managed_display_ownership
)

resume_and_verify() (
    app=$1
    expected_pid=$2
    resume_point=$(checkpoint)
    "$CTL" launch "$app" >/dev/null
    verify_existing_resume "$app" "$expected_pid" "$resume_point"
)

verify_existing_resume() (
    app=$1
    expected_pid=$2
    resume_point=$3
    wait_state "foreground:$app"
    wait_runtime_event "$resume_point" "$app" qtfb-connected
    wait_runtime_event "$resume_point" "$app" first-frame
    wait_runtime_event "$resume_point" "$app" foreground
    assert_runtime_event_order "$resume_point" "$app" qtfb-connected "$app" first-frame
    verify_single_clean_refresh "$app" "$resume_point"
    assert_same_single_process "$app" "$expected_pid"
    # Resuming a parked process must not start a second QProcess/QTFB client.
    assert_no_runtime_event "$resume_point" "$app" starting
    assert_original_runtime
    assert_managed_display_ownership
)

assert_clean_exit_record() (
    point=$1
    app=$2
    expected=$3
    wait_runtime_pattern "$point" \
        "app=$app event=exited exit_code=0 exit_status=normal .*expected=$expected" \
        "$app clean process exit (expected=$expected)"
    exit_line=$(runtime_log_after "$point" \
        | grep -F "app=$app event=exited exit_code=" \
        | sed -n '1p')
    printf '%s\n' "$exit_line" | grep -Fq 'exit_code=0' \
        || fail "$app did not return exit code 0: $exit_line"
    printf '%s\n' "$exit_line" | grep -Fq 'exit_status=normal' \
        || fail "$app exit was not normal: $exit_line"
    printf '%s\n' "$exit_line" | grep -Fq "expected=$expected" \
        || fail "$app exit expectation is wrong (wanted $expected): $exit_line"
)

assert_koreader_graceful_managed_exit() (
    point=$1
    wait_runtime_event "$point" koreader graceful-exit-requested
    wait_runtime_pattern "$point" \
        'remagic-koreader: event=exit-request-dispatched pid=[0-9]+ generation=[0-9]+ ui=(filemanager|reader)' \
        'KOReader native UI exit dispatch'
    assert_clean_exit_record "$point" koreader true
    assert_runtime_pattern_order "$point" \
        'app=koreader event=graceful-exit-requested' 'manager graceful-exit request' \
        'remagic-koreader: event=exit-request-dispatched' 'KOReader native exit dispatch'
    assert_runtime_pattern_order "$point" \
        'remagic-koreader: event=exit-request-dispatched' 'KOReader native exit dispatch' \
        'app=koreader event=exited exit_code=0' 'KOReader clean process exit'
    [ ! -e /run/remagic/koreader-ready ] \
        || fail 'KOReader semantic-ready marker survived clean exit'
    [ ! -e /run/remagic/koreader-exit ] \
        || fail 'KOReader graceful-exit request marker survived clean exit'
)

assert_managed_exit() (
    point=$1
    app=$2
    if [ "$app" = koreader ]; then
        assert_koreader_graceful_managed_exit "$point"
    else
        wait_runtime_pattern "$point" \
            "app=$app event=exited .*exit_status=normal .*expected=true" \
            "$app normal manager-requested exit"
    fi
)

close_background_and_verify() (
    app=$1
    foreground_app=$2
    close_point=$(checkpoint)
    "$CTL" close "$app" --complete >/dev/null
    wait_state "foreground:$foreground_app"
    wait_runtime_event "$close_point" "$app" stopping
    wait_runtime_event "$close_point" "$app" exited
    assert_runtime_event_order "$close_point" "$app" stopping "$app" exited
    wait_managed_process_count "$app" 0
    assert_managed_exit "$close_point" "$app"
    assert_original_runtime
    assert_managed_display_ownership
)

close_and_verify() (
    app=$1
    close_point=$(checkpoint)
    "$CTL" close "$app" --complete >/dev/null
    wait_state manager
    wait_runtime_event "$close_point" "$app" stopping
    wait_runtime_event "$close_point" "$app" exited
    assert_runtime_event_order "$close_point" "$app" stopping "$app" exited
    wait_managed_process_count "$app" 0
    assert_managed_exit "$close_point" "$app"
    assert_original_runtime
    assert_managed_display_ownership
)

close_via_touch_and_verify() (
    app=$1
    x=$2
    close_point=$(checkpoint)
    "$TAP_TOOL" "$x" "$TAP_CLOSE_Y"
    wait_ui_close_pressed "$close_point" "$app"
    wait_state manager
    wait_runtime_event "$close_point" "$app" stopping
    wait_runtime_event "$close_point" "$app" exited
    assert_runtime_event_order "$close_point" "$app" stopping "$app" exited
    wait_managed_process_count "$app" 0
    assert_managed_exit "$close_point" "$app"
    assert_original_runtime
    assert_managed_display_ownership
)

wait_framebuffer_change() (
    framebuffer=$1
    previous_hash=$2
    description=$3
    max_attempts=${4:-80}
    attempts=0
    while [ "$attempts" -lt "$max_attempts" ]; do
        if [ ! -f "$framebuffer" ]; then
            fail "$description framebuffer disappeared before the UI transition completed"
        fi
        current_hash=$(sha256sum "$framebuffer" | awk '{ print $1 }')
        if [ "$current_hash" != "$previous_hash" ]; then
            printf '%s\n' "$current_hash"
            return 0
        fi
        sleep 0.1
        attempts=$((attempts + 1))
    done
    printf '[acceptance] unchanged framebuffer: path=%s hash=%s polls=%s\n' \
        "$framebuffer" "$previous_hash" "$max_attempts" >&2
    fail "$description did not change the KOReader QTFB framebuffer"
)

wait_framebuffer_stable() (
    framebuffer=$1
    description=$2
    max_attempts=${3:-100}
    previous_hash=
    stable_samples=0
    attempts=0
    while [ "$attempts" -lt "$max_attempts" ]; do
        [ -f "$framebuffer" ] \
            || fail "$description framebuffer disappeared while waiting for stable pixels"
        current_hash=$(sha256sum "$framebuffer" | awk '{ print $1 }')
        if [ "$current_hash" = "$previous_hash" ]; then
            stable_samples=$((stable_samples + 1))
            if [ "$stable_samples" -ge 5 ]; then
                printf '%s\n' "$current_hash"
                return 0
            fi
        else
            previous_hash=$current_hash
            stable_samples=0
        fi
        sleep 0.2
        attempts=$((attempts + 1))
    done
    printf '[acceptance] unstable framebuffer: path=%s last_hash=%s polls=%s\n' \
        "$framebuffer" "$previous_hash" "$max_attempts" >&2
    fail "$description framebuffer did not settle"
)

koreader_ui_exit_and_verify() (
    expected_process=$1
    exit_point=$(checkpoint)
    framebuffer=$(single_qtfb_object)
    framebuffer_hash=$(sha256sum "$framebuffer" | awk '{ print $1 }')

    # KOReader v2026.03, zh_CN, 954x1696, default DPI:
    #   1. Tap the empty part of the extended activation zone just inside the
    #      right third, forcing the main tab while avoiding the path bar and
    #      the oversized plus-button target.
    #   2. Tap its eighth row ("退出") to open the exit submenu.
    #   3. Tap the fourth submenu row ("退出").
    # Keep every contact below KOReader's configurable 100 ms minimum hold
    # threshold (the default is 500 ms), so these remain taps for every valid
    # user setting.
    # A fresh uinput node is used for each tap, so retain the normal hotplug
    # delay and give each e-paper menu enough time to paint before the next tap.
    "$TAP_TOOL" "$KOREADER_MENU_X" "$KOREADER_MENU_Y" 1500 50 750
    framebuffer_hash=$(wait_framebuffer_change \
        "$framebuffer" "$framebuffer_hash" 'KOReader main-menu tap')
    framebuffer_hash=$(wait_framebuffer_stable \
        "$framebuffer" 'KOReader main menu')
    verify_visible_qtfb_frame koreader self-exit-main-menu "$framebuffer"
    "$TAP_TOOL" "$KOREADER_EXIT_MENU_X" "$KOREADER_EXIT_MENU_Y" 1500 50 750
    framebuffer_hash=$(wait_framebuffer_change \
        "$framebuffer" "$framebuffer_hash" 'KOReader exit-submenu tap')
    framebuffer_hash=$(wait_framebuffer_stable \
        "$framebuffer" 'KOReader exit submenu')
    verify_visible_qtfb_frame koreader self-exit-exit-menu "$framebuffer"
    "$TAP_TOOL" "$KOREADER_EXIT_X" "$KOREADER_EXIT_Y" 1500 50 750

    wait_runtime_event "$exit_point" koreader exited
    wait_runtime_pattern "$exit_point" \
        'app=koreader event=daemon-exit-notified .*exit_code=0 crashed=false' \
        'KOReader normal self-exit notification'
    wait_state manager
    wait_managed_process_count koreader 0
    wait_path "$framebuffer" absent

    assert_clean_exit_record "$exit_point" koreader false
    assert_no_runtime_event "$exit_point" koreader stopping

    [ ! -e /run/remagic/foreground-app ] \
        || fail 'foreground marker survived KOReader self-exit'
    [ ! -e /home/root/.local/state/remagic/sessions/koreader.json ] \
        || fail 'KOReader session remained after normal self-exit'
    [ "$(process_count koreader)" = 0 ] \
        || fail "KOReader real process group survived self-exit (expected $expected_process)"
    assert_original_runtime
    assert_managed_display_ownership
)

koreader_modal_close_and_verify() (
    expected_process=$1
    framebuffer=$(single_qtfb_object)
    framebuffer_hash=$(sha256sum "$framebuffer" | awk '{ print $1 }')

    # Deliberately leave KOReader inside the New folder dialog with its screen
    # keyboard open.  This used to keep a modal widget in UIManager after the
    # native Exit event, forcing the runtime to kill KOReader after its grace
    # timeout.  A managed close must now drain that modal and still return 0.
    "$TAP_TOOL" "$KOREADER_PLUS_X" "$KOREADER_PLUS_Y" 1500 50 750
    framebuffer_hash=$(wait_framebuffer_change \
        "$framebuffer" "$framebuffer_hash" 'KOReader plus-menu tap')
    framebuffer_hash=$(wait_framebuffer_stable \
        "$framebuffer" 'KOReader plus menu')
    verify_visible_qtfb_frame koreader modal-plus-menu "$framebuffer"

    "$TAP_TOOL" "$KOREADER_NEW_FOLDER_X" "$KOREADER_NEW_FOLDER_Y" 1500 50 750
    framebuffer_hash=$(wait_framebuffer_change \
        "$framebuffer" "$framebuffer_hash" 'KOReader New folder tap')
    wait_framebuffer_stable "$framebuffer" 'KOReader New folder dialog' >/dev/null
    verify_visible_qtfb_frame koreader modal-new-folder "$framebuffer"

    park_and_verify koreader "$expected_process"
    close_point=$(checkpoint)
    "$CTL" close koreader --complete >/dev/null
    wait_state manager
    wait_runtime_event "$close_point" koreader stopping
    wait_runtime_event "$close_point" koreader exited
    assert_runtime_event_order "$close_point" koreader stopping koreader exited
    wait_runtime_pattern "$close_point" \
        'remagic-koreader: event=exit-drain-forced .*ui=filemanager' \
        'KOReader modal UI drain'
    wait_managed_process_count koreader 0
    wait_path "$framebuffer" absent
    assert_managed_exit "$close_point" koreader
    assert_original_runtime
    assert_managed_display_ownership
)

single_qtfb_object() (
    set -- /dev/shm/qtfb_*
    [ -e "$1" ] || fail 'active application has no QTFB shared-memory object'
    [ "$#" -eq 1 ] || fail "expected one active QTFB object, found $#"
    printf '%s\n' "$1"
)

verify_visible_qtfb_frame() (
    app=$1
    label=$2
    framebuffer=${3:-}
    frame_policy=${4:-visible}
    if [ -z "$framebuffer" ]; then
        framebuffer=$(single_qtfb_object)
    fi
    [ -f "$framebuffer" ] || fail "$app framebuffer does not exist: $framebuffer"
    mkdir -p "$FRAME_EVIDENCE_DIR"
    diagnostic="$FRAME_EVIDENCE_DIR/$app-$label.rgb565"
    cp "$framebuffer" "$diagnostic"
    raw_bytes=$(wc -c < "$diagnostic" | tr -d '[:space:]')
    compressed_bytes=$(gzip -c "$diagnostic" | wc -c | tr -d '[:space:]')
    expected_bytes=$((QTFB_WIDTH * QTFB_HEIGHT * 2))
    [ "$raw_bytes" = "$expected_bytes" ] \
        || fail "$app framebuffer has the wrong RGB565 geometry ($raw_bytes bytes, expected $expected_bytes)"
    # A uniform 954x1696 RGB565 frame compresses to roughly 3 KiB. Requiring
    # more than 6 KiB proves that the client supplied real UI pixels rather
    # than merely connecting QTFB and submitting a blank first frame.
    if [ "$frame_policy" != blank-ok ]; then
        [ "$compressed_bytes" -gt 6000 ] \
            || fail "$app framebuffer appears blank (gzip=$compressed_bytes; saved=$diagnostic)"
    fi
    frame_hash=$(sha256sum "$diagnostic" | awk '{ print $1 }')
    printf '%s  %s\n' "$frame_hash" "$(basename "$diagnostic")" \
        > "$diagnostic.sha256"
    printf '[acceptance] %s framebuffer (%s): source=%s geometry=%sx%s raw=%s gzip=%s sha256=%s saved=%s\n' \
        "$app" "$frame_policy" \
        "$framebuffer" "$QTFB_WIDTH" "$QTFB_HEIGHT" "$raw_bytes" \
        "$compressed_bytes" "$frame_hash" "$diagnostic"
)

wait_input_device_named() (
    expected_name=$1
    attempts=0
    while [ "$attempts" -lt 120 ]; do
        for name_path in /sys/class/input/event*/device/name; do
            [ -r "$name_path" ] || continue
            actual_name=$(tr -d '\r\n' < "$name_path")
            if [ "$actual_name" = "$expected_name" ]; then
                event_name=${name_path#/sys/class/input/}
                event_name=${event_name%%/*}
                printf '/dev/input/%s\n' "$event_name"
                return 0
            fi
        done
        sleep 0.1
        attempts=$((attempts + 1))
    done
    fail "input device did not appear: $expected_name"
)

runtime_holds_device() (
    device_path=$1
    for descriptor in "/proc/$RUNTIME_PID/fd/"*; do
        [ -e "$descriptor" ] || continue
        [ "$(readlink "$descriptor" 2>/dev/null || true)" = "$device_path" ] \
            && return 0
    done
    return 1
)

assert_runtime_holds_no_marker() (
    for name_path in /sys/class/input/event*/device/name; do
        [ -r "$name_path" ] || continue
        actual_name=$(tr -d '\r\n' < "$name_path")
        printf '%s\n' "$actual_name" | grep -qi marker || continue
        event_name=${name_path#/sys/class/input/}
        event_name=${event_name%%/*}
        if runtime_holds_device "/dev/input/$event_name"; then
            fail "background runtime still holds marker $actual_name (/dev/input/$event_name)"
        fi
    done
)

verify_magicpaper_pen_pipeline() (
    expected_pid=$1
    framebuffer=$(single_qtfb_object)

    # The QTFB connection remains active while parked, so this explicitly
    # proves that foreground ownership (not SHM connection state) controls the
    # marker descriptor.
    park_point=$(checkpoint)
    park_and_verify magicpaper "$expected_pid"
    wait_runtime_pattern "$park_point" 'event=raw-pen-close ' \
        'raw marker closed when MagicPaper entered the background'
    assert_runtime_holds_no_marker
    background_pen_point=$(checkpoint)
    "$TAP_TOOL" --pen-line 100 500 300 700 100 120 100
    if runtime_log_after "$background_pen_point" \
        | grep -Eq 'magic-paper: event=pen-session-(start|finish) ';
    then
        fail 'background MagicPaper consumed a virtual marker stroke'
    fi
    assert_runtime_holds_no_marker

    # Hot-plug the deterministic marker while parked.  The long pre-draw
    # delay leaves time to resume MagicPaper; the foreground transition then
    # rescans input devices and must prefer this marker over the real Elan.
    point=$(checkpoint)
    "$TAP_TOOL" --pen-equation 8000 120 1000 &
    injector_pid=$!
    acceptance_marker=$(wait_input_device_named 'Remagic acceptance marker')
    resume_point=$(checkpoint)
    "$CTL" launch magicpaper >/dev/null
    verify_existing_resume magicpaper "$expected_pid" "$resume_point"
    wait_runtime_pattern "$resume_point" \
        'event=raw-pen-open .*acceptance_device=true' \
        'foreground runtime selected the acceptance marker'
    runtime_holds_device "$acceptance_marker" \
        || fail "foreground runtime did not hold $acceptance_marker"

    before=$(sha256sum "$framebuffer" | awk '{ print $1 }')
    # One meaningless diagonal is often (correctly) rejected as empty OCR.
    # Draw a large multi-stroke `1+1=` in one pen-device lifetime so this is a
    # real end-to-end handwriting/OCR/answer smoke test rather than a network
    # request forced from arbitrary pixels.
    if ! wait "$injector_pid"; then
        fail 'acceptance marker failed while drawing 1+1='
    fi
    wait_runtime_pattern "$point" 'magic-paper: event=pen-session-start .*source=qtfb' 'QTFB pen press'
    wait_runtime_pattern "$point" 'magic-paper: event=local-ink-presented ' 'visible local ink'
    wait_runtime_pattern "$point" 'magic-paper: event=pen-session-finish .*releases=[1-9]' 'QTFB pen release'
    after=$(sha256sum "$framebuffer" | awk '{ print $1 }')
    [ "$before" != "$after" ] || fail 'MagicPaper framebuffer did not change after injected pen stroke'
    wait_runtime_pattern "$point" 'magic-paper: event=idle-commit ' 'idle commit' 400
    wait_runtime_pattern "$point" 'magic-paper: event=ocr-submit ' 'OCR submission' 400
    wait_runtime_pattern "$point" 'magic-paper: event=ocr-done ' 'OCR completion' 1200
    wait_runtime_pattern "$point" 'magic-paper: event=(llm-done|llm-skipped) ' 'LLM terminal result' 1200
    wait_runtime_pattern "$point" \
        'magic-paper: event=turn-render-complete kind=user reply_chars=[1-9][0-9]* ' \
        'rendered and persisted MagicPaper turn' 1200
)

section 'normalize to the stock system domain'
[ -x "$CTL" ] || fail "remagicctl is missing or not executable: $CTL"
[ -s /home/root/apps/remagic/share/build-info.txt ] \
    || fail 'deployed build-info.txt is missing or empty'
for component in built-at remagic-manager magicpaper koreader-adapter koreader appload; do
    grep -q "^$component=" /home/root/apps/remagic/share/build-info.txt \
        || fail "deployed build-info.txt has no $component entry"
done
printf '[acceptance] deployed build\n'
sed 's/^/[acceptance]   /' /home/root/apps/remagic/share/build-info.txt
for font in \
    STDongGuanTi-Regular.ttf \
    STDongGuanTi-Bold.ttf \
    STDongGuanTi-Light.ttf \
    FZPingXianYaSong.ttf; do
    [ -s "/home/root/apps/koreader/fonts/remagic/$font" ] \
        || fail "KOReader custom font is missing or empty: $font"
done
"$CTL" status >/dev/null
"$CTL" system >/dev/null
wait_state system
wait_service xochitl.service active
if unit_loaded paperweight.service; then
    wait_service paperweight.service active
fi
wait_service "$RUNTIME_UNIT" inactive
if unit_loaded magicpaper-agent.service; then
    # A previous MagicPaper session may intentionally have left its heartbeat
    # worker alive. Acceptance starts from a deterministic, completely closed
    # application baseline.
    systemctl stop magicpaper-agent.service
    wait_service magicpaper-agent.service inactive
fi
wait_managed_process_count magicpaper 0
wait_managed_process_count koreader 0

section 'system -> manager through the single Qt runtime'
"$CTL" manager >/dev/null
wait_state manager
wait_service "$RUNTIME_UNIT" active
wait_service xochitl.service inactive
if unit_loaded paperweight.service; then
    wait_service paperweight.service inactive
fi
wait_path "$CONTROL_SOCKET" present
wait_path "$QTFB_SOCKET" present
assert_single_runtime
assert_no_legacy_hosts
RUNTIME_PID=$(single_runtime_pid)
XOCHITL_GENERATION=$(unit_activation_generation xochitl.service)
PAPERWEIGHT_GENERATION=$(unit_activation_generation paperweight.service)
assert_managed_display_ownership

if [ -x "$TAP_TOOL" ]; then
    section 'real touchscreen injection selects the first manager card'
    touch_point=$(checkpoint)
    "$TAP_TOOL" "$TAP_FIRST_CARD_X" "$TAP_FIRST_CARD_Y"
    touched_app=$(wait_ui_app_pressed "$touch_point")
    printf '[acceptance] touchscreen selected %s\n' "$touched_app"
    verify_new_launch "$touched_app" "$touch_point"
    touched_pid=$(single_real_pid "$touched_app")
    park_and_verify "$touched_app" "$touched_pid"
    close_via_touch_and_verify "$touched_app" "$TAP_FIRST_CLOSE_X"

    section 'real touchscreen injection selects the second manager card'
    second_touch_point=$(checkpoint)
    "$TAP_TOOL" "$TAP_SECOND_CARD_X" "$TAP_SECOND_CARD_Y"
    second_app=$(wait_ui_app_pressed "$second_touch_point")
    [ "$second_app" != "$touched_app" ] \
        || fail "both manager card coordinates selected $second_app"
    printf '[acceptance] second touchscreen card selected %s\n' "$second_app"
    verify_new_launch "$second_app" "$second_touch_point"
    second_pid=$(single_real_pid "$second_app")
    park_and_verify "$second_app" "$second_pid"
    close_via_touch_and_verify "$second_app" "$TAP_SECOND_CLOSE_X"
else
    fail "touchscreen injection tool is unavailable: $TAP_TOOL"
fi

section 'MagicPaper launch, first frame, singleton, park, resume and close'
magicpaper_point=$(checkpoint)
"$CTL" launch magicpaper >/dev/null
verify_new_launch magicpaper "$magicpaper_point"
magicpaper_pid=$(single_real_pid magicpaper)
verify_duplicate_launch magicpaper "$magicpaper_pid"
verify_magicpaper_pen_pipeline "$magicpaper_pid"

section 'direct MagicPaper -> KOReader -> MagicPaper foreground switching'
magicpaper_framebuffer=$(single_qtfb_object)
switch_to_koreader_point=$(checkpoint)
"$CTL" launch koreader >/dev/null
verify_new_launch koreader "$switch_to_koreader_point"
wait_runtime_event "$switch_to_koreader_point" magicpaper background
assert_same_single_process magicpaper "$magicpaper_pid"
koreader_switch_pid=$(single_real_pid koreader)
koreader_switch_framebuffer=
for framebuffer in /dev/shm/qtfb_*; do
    if [ -e "$framebuffer" ] && [ "$framebuffer" != "$magicpaper_framebuffer" ]; then
        [ -z "$koreader_switch_framebuffer" ] \
            || fail 'direct switch created more than one additional QTFB framebuffer'
        koreader_switch_framebuffer=$framebuffer
    fi
done
[ -n "$koreader_switch_framebuffer" ] \
    || fail 'direct switch did not create a KOReader QTFB framebuffer'
koreader_switch_hash=$(wait_framebuffer_stable \
    "$koreader_switch_framebuffer" 'initial KOReader library')
verify_visible_qtfb_frame koreader direct-switch "$koreader_switch_framebuffer"

switch_to_magicpaper_point=$(checkpoint)
"$CTL" launch magicpaper >/dev/null
wait_runtime_event "$switch_to_magicpaper_point" koreader background
verify_existing_resume magicpaper "$magicpaper_pid" "$switch_to_magicpaper_point"
assert_same_single_process koreader "$koreader_switch_pid"
# MagicPaper intentionally dissolves a completed reply after its linger
# interval. KOReader startup can outlast that interval, so a white canvas is a
# valid resumed UI here; geometry, lifecycle and later pen tests carry the
# stronger evidence that this is the live app rather than a missing frame.
verify_visible_qtfb_frame magicpaper direct-switch-recall "$magicpaper_framebuffer" blank-ok

switch_back_to_koreader_point=$(checkpoint)
"$CTL" launch koreader >/dev/null
wait_runtime_event "$switch_back_to_koreader_point" magicpaper background
verify_existing_resume koreader "$koreader_switch_pid" "$switch_back_to_koreader_point"
assert_same_single_process magicpaper "$magicpaper_pid"
[ -f "$koreader_switch_framebuffer" ] \
    || fail 'KOReader QTFB framebuffer was replaced during direct recall'
koreader_recalled_hash=$(wait_framebuffer_stable \
    "$koreader_switch_framebuffer" 'recalled KOReader library')
[ "$koreader_recalled_hash" = "$koreader_switch_hash" ] \
    || fail "KOReader visible page changed across background recall ($koreader_switch_hash -> $koreader_recalled_hash)"
verify_visible_qtfb_frame koreader direct-switch-recall "$koreader_switch_framebuffer"

switch_again_to_magicpaper_point=$(checkpoint)
"$CTL" launch magicpaper >/dev/null
wait_runtime_event "$switch_again_to_magicpaper_point" koreader background
verify_existing_resume magicpaper "$magicpaper_pid" "$switch_again_to_magicpaper_point"
assert_same_single_process koreader "$koreader_switch_pid"
close_background_and_verify koreader magicpaper

park_and_verify magicpaper "$magicpaper_pid"
resume_and_verify magicpaper "$magicpaper_pid"
park_and_verify magicpaper "$magicpaper_pid"
close_and_verify magicpaper
if unit_loaded magicpaper-agent.service; then
    wait_service magicpaper-agent.service inactive
fi

section 'KOReader internal UI exit returns code 0 and clears runtime state'
koreader_self_exit_point=$(checkpoint)
"$CTL" launch koreader >/dev/null
verify_new_launch koreader "$koreader_self_exit_point"
verify_visible_qtfb_frame koreader self-exit
koreader_self_exit_pid=$(single_real_pid koreader)
koreader_ui_exit_and_verify "$koreader_self_exit_pid"

section 'KOReader modal dialog closes gracefully through the manager'
koreader_modal_point=$(checkpoint)
"$CTL" launch koreader >/dev/null
verify_new_launch koreader "$koreader_modal_point"
verify_visible_qtfb_frame koreader modal-before-dialog
koreader_modal_pid=$(single_real_pid koreader)
koreader_modal_close_and_verify "$koreader_modal_pid"

section 'KOReader three complete real-device lifecycle rounds'
round=1
while [ "$round" -le 3 ]; do
    printf '[acceptance] KOReader round %s/3\n' "$round"
    koreader_point=$(checkpoint)
    "$CTL" launch koreader >/dev/null
    verify_new_launch koreader "$koreader_point"
    verify_visible_qtfb_frame koreader "round-$round"
    if [ "$round" -eq 1 ]; then
        wait_runtime_pattern "$koreader_point" \
            'koreader-remagic: library_dir=/home/root/\.local/share/remagic-koreader/library(/[^ ]*)? source=(lastdir|fallback)' \
            'KOReader friendly library directory'
        if runtime_log_after "$koreader_point" \
            | grep -Eq 'koreader-remagic: library_dir=/home/root/\.local/share/remarkable/xochitl'; then
            fail 'KOReader fell back to the raw xochitl metadata directory'
        fi
    fi
    koreader_pid=$(single_real_pid koreader)
    verify_duplicate_launch koreader "$koreader_pid"
    park_and_verify koreader "$koreader_pid"
    resume_and_verify koreader "$koreader_pid"
    park_and_verify koreader "$koreader_pid"
    close_and_verify koreader
    round=$((round + 1))
done

section 'return to stock gracefully closes a parked KOReader instance'
shutdown_koreader_launch_point=$(checkpoint)
"$CTL" launch koreader >/dev/null
verify_new_launch koreader "$shutdown_koreader_launch_point"
shutdown_koreader_pid=$(single_real_pid koreader)
verify_visible_qtfb_frame koreader before-system-handoff
park_and_verify koreader "$shutdown_koreader_pid"

if [ -x "$TAP_TOOL" ]; then
    system_touch_point=$(checkpoint)
    "$TAP_TOOL" "$TAP_SYSTEM_X" "$TAP_SYSTEM_Y"
    wait_ui_system_pressed "$system_touch_point"
else
    fail "Back-button touch cannot be tested because the injection tool is unavailable: $TAP_TOOL"
fi
assert_koreader_graceful_managed_exit "$system_touch_point"
wait_managed_process_count koreader 0
wait_state system
wait_service "$RUNTIME_UNIT" inactive
wait_service xochitl.service active
if unit_loaded paperweight.service; then
    wait_service paperweight.service active
fi
wait_managed_process_count magicpaper 0
wait_managed_process_count koreader 0
wait_path "$CONTROL_SOCKET" absent
wait_path "$QTFB_SOCKET" absent
[ "$(runtime_process_count)" = 0 ] || fail 'runtime process survived the system handoff'
assert_no_legacy_hosts

section 'failed units, crashes, core dumps and stale QTFB resources'
failed_units=$(systemctl --failed --no-legend --no-pager 2>/dev/null \
    | grep -E 'remagic|magicpaper|xochitl|paperweight' || true)
[ -z "$failed_units" ] || fail "relevant failed units remain: $failed_units"

if journal_after "$TEST_CHECKPOINT" -u remagicd.service -u "$RUNTIME_UNIT" -o cat \
    | grep -Eiq 'event=crashed|segmentation fault|segfault|core dump(ed)?|dumped core|status=11/SEGV|signal=SEGV|status=6/ABRT|signal=ABRT'; then
    fail 'a managed component crashed during this acceptance run'
fi

runtime_errors=$(journal_after "$TEST_CHECKPOINT" \
    -u remagicd.service -u "$RUNTIME_UNIT" -o cat \
    | grep -Ei 'TypeError:|indexOf.*undefined|QObject: Cannot create children|QSocketNotifier: Invalid socket|Unknown QTFB framebuffer type|QTFB.*(invalid|unsupported|unknown).*(mode|type)|Failed to (open|mmap|resize|send|receive).*QTFB|semantic_ready_timeout|event=semantic-ready-failed|event=graceful-exit-timeout|Failed to initialize statistics plugin|no such table: page_stat|Failed to lock epframebuffer|stop-sigterm timed out' \
    || true)
[ -z "$runtime_errors" ] || fail "runtime errors were recorded: $runtime_errors"

if command -v coredumpctl >/dev/null 2>&1; then
    relevant_cores=$(coredumpctl --since="@$TEST_STARTED_AT" --no-pager --no-legend list 2>/dev/null \
        | grep -Ei 'remagic|appload|magicpaper|riddle|koreader|reader.lua|luajit' || true)
    [ -z "$relevant_cores" ] || fail "relevant core dumps were recorded: $relevant_cores"
fi

loose_cores=
for core_file in /home/root/core /home/root/core.* /tmp/core /tmp/core.*; do
    [ -f "$core_file" ] || continue
    loose_cores="$loose_cores $core_file"
done
[ -z "$loose_cores" ] || fail "loose core files remain on the device: $loose_cores"

set -- /dev/shm/qtfb_*
if [ -e "$1" ]; then
    fail 'stale QTFB shared-memory objects remain after returning to stock'
fi

if [ "$MAGICPAPER_AGENT_WAS_ACTIVE" = true ]; then
    systemctl start magicpaper-agent.service
    wait_service magicpaper-agent.service active
fi

printf '\nACCEPTANCE_OK\n'
