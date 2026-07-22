#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}
TEST_STARTED_AT=$(date +%s)
ISOLATION=${REMAGIC_TEST_ISOLATION:-/home/root/apps/remagic/libexec/device-test-isolation.sh}
REMAGIC_TEST_CTL=${REMAGIC_TEST_CTL:-$CTL}
[ -r "$ISOLATION" ] || { echo "[v2-acceptance] missing isolation helper: $ISOLATION" >&2; exit 1; }
# shellcheck source=scripts/lib/device-test-isolation.sh
. "$ISOLATION"

fail() {
    echo "[v2-acceptance] ERROR: $*" >&2
    exit 1
}

diagnostics() {
    echo "[v2-acceptance] manager status" >&2
    "$CTL" status >&2 || true
    echo "[v2-acceptance] display status" >&2
    "$CTL" display-status >&2 || true
    echo "[v2-acceptance] recent panel submissions" >&2
    "$CTL" display-submissions >&2 || true
    echo "[v2-acceptance] units" >&2
    systemctl status --no-pager remagicd.service remagic-display-host.service \
        remagic-home.service 'remagic-app@magicpaper.service' \
        'remagic-app@koreader.service' xochitl.service paperweight.service >&2 || true
    echo "[v2-acceptance] journal" >&2
    journalctl --since="@$TEST_STARTED_AT" --no-pager \
        -u remagicd.service -u remagic-display-host.service \
        -u remagic-home.service -u 'remagic-app@magicpaper.service' \
        -u 'remagic-app@koreader.service' -o short-monotonic | tail -n 500 >&2 || true
}

cleanup() {
    local cleanup_status
    [ "$#" -eq 1 ] || exit 1
    cleanup_status=$1
    trap - EXIT HUP INT TERM
    if [ "$cleanup_status" -ne 0 ]; then
        diagnostics
        "$CTL" system >/dev/null 2>&1 \
            || /home/root/apps/remagic/libexec/remagic-recover >/dev/null 2>&1 \
            || true
    fi
    remagic_test_finish || cleanup_status=1
    exit "$cleanup_status"
}
trap 'cleanup "$?"' EXIT
trap 'exit 1' HUP INT TERM

wait_unit() {
    local unit wanted attempts actual
    [ "$#" -eq 2 ] || return 1
    unit=$1
    wanted=$2
    attempts=0
    while [ "$attempts" -lt 160 ]; do
        actual=$(systemctl is-active "$unit" 2>/dev/null || true)
        [ "$actual" = "$wanted" ] && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    fail "$unit did not become $wanted"
}

wait_domain() {
    local pattern attempts status
    [ "$#" -eq 1 ] || return 1
    pattern=$1
    attempts=0
    while [ "$attempts" -lt 240 ]; do
        status=$("$CTL" status 2>/dev/null || true)
        printf '%s' "$status" | grep -q "$pattern" && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    fail "manager domain did not match $pattern"
}

display_status() {
    "$CTL" display-status
}

display_number() {
    local field
    [ "$#" -eq 1 ] || return 1
    field=$1
    display_status | sed -n "s/.*\"$field\": \([0-9][0-9]*\).*/\1/p" | sed -n '1p'
}

display_submissions() {
    "$CTL" display-submissions
}

last_submission_sequence() {
    local submissions
    submissions=$(display_submissions) || fail "could not read panel submission evidence"
    printf '%s\n' "$submissions" | awk -F '\t' '
        NR == 1 {
            if (NF != 10 || $1 != "sequence" || $2 != "surface_sequence" ||
                $3 != "key" || $4 != "generation" || $5 != "foreground_epoch" ||
                $6 != "intent" || $7 != "reason" || $8 != "visible_signature" ||
                $9 != "marker" || $10 != "success") exit 2
            next
        }
        { last = $1 }
        END { if (last == "") print 0; else print last }
    '
}

assert_foreground_submission_since() {
    local baseline key generation epoch intent label submissions count valid
    [ "$#" -eq 6 ] || return 1
    baseline=$1 key=$2 generation=$3 epoch=$4 intent=$5 label=$6
    wait_panel_settled
    submissions=$(display_submissions)
    count=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" \
        -v epoch="$epoch" -v intent="$intent" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $6 == intent &&
                $7 == "foreground_switch" { count++ }
            END { print count + 0 }
        ')
    [ "$count" -eq 1 ] \
        || fail "$label has $count matching $intent foreground submissions instead of one"
    valid=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" \
        -v epoch="$epoch" -v intent="$intent" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            function unz(v) { return v ~ /^[0-9]+$/ && v ~ /[1-9]/ }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $6 == intent &&
                $7 == "foreground_switch" && unz($2) && unz($8) &&
                unz($9) && $10 == "true" { count++ }
            END { print count + 0 }
        ')
    [ "$valid" -eq 1 ] || fail "$label foreground submission lacks successful panel evidence"
    MATCHED_SUBMISSION_SEQUENCE=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" \
        -v epoch="$epoch" -v intent="$intent" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $6 == intent &&
                $7 == "foreground_switch" { print $1 }
        ')
}

assert_no_lease_success_after() {
    local sequence key generation epoch label submissions count
    [ "$#" -eq 5 ] || return 1
    sequence=$1 key=$2 generation=$3 epoch=$4 label=$5
    submissions=$(display_submissions) || fail "could not read panel submission evidence"
    count=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v sequence="$sequence" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            NR > 1 && ugt($1, sequence) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $10 == "true" { count++ }
            END { print count + 0 }
        ')
    [ "$count" -eq 0 ] || fail "$label accepted $count stale-lease panel submissions"
}

assert_home_press_release_since() {
    local baseline key generation epoch submissions count valid submission_sequences signatures
    [ "$#" -eq 4 ] || return 1
    baseline=$1 key=$2 generation=$3 epoch=$4
    submissions=$(display_submissions)
    count=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $7 == "surface_damage" { count++ }
            END { print count + 0 }
        ')
    [ "$count" -eq 2 ] || fail "Home tap produced $count damage submissions instead of press/release"
    valid=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            function unz(v) { return v ~ /^[0-9]+$/ && v ~ /[1-9]/ }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $7 == "surface_damage" &&
                unz($2) && unz($8) && unz($9) && $10 == "true" { count++ }
            END { print count + 0 }
        ')
    [ "$valid" -eq 2 ] || fail "Home press/release lacks successful panel evidence"
    submission_sequences=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) &&
                $7 == "surface_damage" { if (first == "") first=$1; else last=$1 }
            END { print first " " last }
        ')
    set -- $submission_sequences
    [ "$1" != "$2" ] || fail "Home press/release reused one panel submission"
    signatures=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) &&
                $7 == "surface_damage" { if (first == "") first=$8; else last=$8 }
            END { print first " " last }
        ')
    set -- $signatures
    [ "$1" != "$2" ] || fail "Home press/release has indistinguishable visible signatures"
}

surface_sequence() {
    local key sequence
    [ "$#" -eq 1 ] || return 1
    key=$1
    sequence=$(display_status | sed -n '/"surface_sequences":[[:space:]]*{/,/^[[:space:]]*}/ {
        s/^[[:space:]]*"'"$key"'":[[:space:]]*\([0-9][0-9]*\),*$/\1/p
    }' | sed -n '1p')
    [ -n "$sequence" ] || fail "display status has no sequence for surface $key"
    printf '%s\n' "$sequence"
}

assert_ink_dissolve_since() {
    local baseline baseline_surface baseline_full key generation epoch label
    local attempts submissions live_sequence terminal_sequence current_surface
    local canonical_settles full_submissions terminal_valid
    [ "$#" -eq 7 ] || return 1
    baseline=$1 baseline_surface=$2 baseline_full=$3 key=$4
    generation=$5 epoch=$6 label=$7
    attempts=0
    # The idle commit starts after roughly 2.6 seconds and the dissolve then
    # runs for about 0.7 seconds. Standard cleanup ends in mono_quality while
    # the user-selectable enhanced cleanup uses the content waveform. Both are
    # bounded partial refreshes; a network failure after them is unrelated.
    while [ "$attempts" -lt 240 ]; do
        submissions=$(display_submissions)
        live_sequence=$(printf '%s\n' "$submissions" | awk -F '\t' \
            -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
                function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
                function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
                function ugt(a, b, ca, cb, la, lb) {
                    ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                    if (la != lb) return la > lb
                    return ("u" ca) > ("u" cb)
                }
                function unz(v) { return v ~ /^[0-9]+$/ && v ~ /[1-9]/ }
                NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                    ueq($4, generation) && ueq($5, epoch) && $6 == "ink" &&
                    $7 == "live_ink" && unz($8) && unz($9) &&
                    $10 == "true" { print $1; exit }
            ')
        terminal_sequence=$(printf '%s\n' "$submissions" | awk -F '\t' \
            -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
                function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
                function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
                function ugt(a, b, ca, cb, la, lb) {
                    ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                    if (la != lb) return la > lb
                    return ("u" ca) > ("u" cb)
                }
                function unz(v) { return v ~ /^[0-9]+$/ && v ~ /[1-9]/ }
                NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                    ueq($4, generation) && ueq($5, epoch) &&
                    ($6 == "mono_quality" || $6 == "content") &&
                    $7 == "surface_damage" && unz($2) && unz($8) &&
                    unz($9) && $10 == "true" { print $1; exit }
            ')
        current_surface=$(surface_sequence "$key")
        if [ -n "$live_sequence" ] && [ -n "$terminal_sequence" ] &&
            remagic_test_u64_greater "$terminal_sequence" "$live_sequence" &&
            remagic_test_u64_greater "$current_surface" "$baseline_surface"; then
            wait_panel_settled
            submissions=$(display_submissions)
            current_surface=$(surface_sequence "$key")
            remagic_test_u64_greater "$current_surface" "$baseline_surface" \
                || fail "$label canonical surface sequence did not advance"
            canonical_settles=$(printf '%s\n' "$submissions" | awk -F '\t' \
                -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
                    function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
                    function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
                    function ugt(a, b, ca, cb, la, lb) {
                        ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                        if (la != lb) return la > lb
                        return ("u" ca) > ("u" cb)
                    }
                    NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                        ueq($4, generation) && ueq($5, epoch) &&
                        $7 == "canonical_settle" { count++ }
                    END { print count + 0 }
                ')
            [ "$canonical_settles" -eq 0 ] \
                || fail "$label produced $canonical_settles forbidden canonical-settle submissions"
            full_submissions=$(printf '%s\n' "$submissions" | awk -F '\t' \
                -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
                    function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
                    function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
                    function ugt(a, b, ca, cb, la, lb) {
                        ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                        if (la != lb) return la > lb
                        return ("u" ca) > ("u" cb)
                    }
                    NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                        ueq($4, generation) && ueq($5, epoch) && $6 == "full" { count++ }
                    END { print count + 0 }
                ')
            [ "$full_submissions" -eq 0 ] \
                || fail "$label produced $full_submissions forbidden full submissions"
            [ "$(display_number full_refresh_count)" = "$baseline_full" ] \
                || fail "$label changed the hardware full-refresh count"
            terminal_valid=$(printf '%s\n' "$submissions" | awk -F '\t' \
                -v sequence="$terminal_sequence" -v key="$key" \
                -v generation="$generation" -v epoch="$epoch" '
                    function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
                    function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
                    NR > 1 && ueq($1, sequence) && ueq($3, key) &&
                        ueq($4, generation) && ueq($5, epoch) &&
                        ($6 == "mono_quality" || $6 == "content") &&
                        $7 == "surface_damage" &&
                        $10 == "true" { count++ }
                    END { print count + 0 }
                ')
            [ "$terminal_valid" -eq 1 ] \
                || fail "$label lost its terminal quality-partial dissolve evidence"
            return 0
        fi
        sleep 0.05
        attempts=$((attempts + 1))
    done
    fail "$label did not produce live ink and a terminal quality-partial dissolve"
}

foreground_key() {
    display_number foreground_key
}

main_pid() {
    systemctl show --property=MainPID --value "$1"
}

assert_freezer_state() {
    local unit expected actual pid process_state
    [ "$#" -eq 2 ] || return 1
    unit=$1
    expected=$2
    actual=$(systemctl show --property=FreezerState --value "$unit" 2>/dev/null || true)
    [ "$actual" = "$expected" ] && return 0
    # The Move kernel/systemd combination may expose no cgroup freezer even on
    # systemd 255. Manager then freezes the complete unit with SIGSTOP and
    # fences it on the runner's real process state.
    pid=$(main_pid "$unit")
    process_state=$(sed -n 's/^State:[[:space:]]*\([^[:space:]]\).*/\1/p' \
        "/proc/$pid/status" 2>/dev/null || true)
    case "$expected:$actual:$process_state" in
        frozen:running:T|frozen:running:t) return 0 ;;
        running:running:T|running:running:t) ;;
        running:running:?) return 0 ;;
    esac
    fail "$unit freezer/process state is $actual/$process_state instead of $expected"
}

surface_present() {
    local key
    [ "$#" -eq 1 ] || return 1
    key=$1
    display_status | sed -n '/"surfaces":[[:space:]]*\[/,/^[[:space:]]*],*$/p' \
        | grep -Eq "^[[:space:]]*$key,?$"
}

wait_surface_absent() {
    local key attempts
    [ "$#" -eq 1 ] || return 1
    key=$1
    attempts=0
    while [ "$attempts" -lt 100 ]; do
        surface_present "$key" || return 0
        sleep 0.05
        attempts=$((attempts + 1))
    done
    fail "surface $key remained registered"
}

wait_panel_settled() {
    local attempts stable
    attempts=0
    stable=0
    while [ "$attempts" -lt 160 ]; do
        if [ "$(display_number queue_depth)" = 0 ]; then
            stable=$((stable + 1))
            [ "$stable" -ge 4 ] && return 0
        else
            stable=0
        fi
        sleep 0.05
        attempts=$((attempts + 1))
    done
    fail "panel command queue did not reach a stable idle window"
}

full_refresh_checkpoint() {
    wait_panel_settled
    display_number full_refresh_count
}

surface_signature() {
    local key
    [ "$#" -eq 1 ] || return 1
    key=$1
    "$CTL" display-signature "$key"
}

assert_presented_surface() {
    local key signature
    [ "$#" -eq 1 ] || return 1
    key=$1
    signature=$(surface_signature "$key")
    remagic_test_u64_nonzero "$signature" \
        || fail "surface $key has no non-empty content signature"
    [ "$(display_number last_presented_key)" = "$key" ] \
        || fail "panel telemetry was not produced by surface $key"
    remagic_test_u64_nonzero "$(display_number last_presented_sequence)" \
        || fail "surface $key has no presented frame sequence"
}

assert_one_full_refresh_since() {
    local before label after
    [ "$#" -eq 2 ] || return 1
    before=$1
    label=$2
    wait_panel_settled
    after=$(display_number full_refresh_count)
    remagic_test_u64_is_next "$before" "$after" \
        || fail "$label full-refresh count was $before then $after instead of one increment"
}

lifecycle_value() {
    local app field
    [ "$#" -eq 2 ] || return 1
    app=$1
    field=$2
    sed -n "s/.*\"$field\":[[:space:]]*\([0-9][0-9]*\).*/\1/p" \
        "/run/remagic/apps/$app/lifecycle-status.json" | sed -n '1p'
}

lifecycle_event() {
    local app
    [ "$#" -eq 1 ] || return 1
    app=$1
    sed -n 's/.*"event":[[:space:]]*"\([^"]*\)".*/\1/p' \
        "/run/remagic/apps/$app/lifecycle-status.json" | sed -n '1p'
}

assert_ready_fence() {
    local app status generation epoch first_frame
    [ "$#" -eq 1 ] || return 1
    app=$1
    status=/run/remagic/apps/$app/lifecycle-status.json
    [ -s "$status" ] || fail "$app did not publish lifecycle status"
    [ "$(lifecycle_event "$app")" = ready ] || fail "$app lifecycle event is not ready"
    generation=$(display_number generation)
    epoch=$(display_number foreground_epoch)
    [ "$(lifecycle_value "$app" generation)" = "$generation" ] \
        || fail "$app lifecycle/display generation mismatch"
    [ "$(lifecycle_value "$app" foreground_epoch)" = "$epoch" ] \
        || fail "$app lifecycle/display epoch mismatch"
    [ "$(lifecycle_value "$app" lease_id)" = "$epoch" ] \
        || fail "$app lifecycle lease is not the active foreground lease"
    first_frame=$(lifecycle_value "$app" first_frame_sequence)
    if [ -n "$first_frame" ] && ! remagic_test_u64_nonzero "$first_frame"; then
        fail "$app published an invalid first-frame sequence"
    fi
}

remagic_test_begin acceptance || fail "could not establish isolated application data"

echo "[v2-acceptance] stock baseline"
wait_domain '"domain": "system"'
wait_unit xochitl.service active
wait_unit remagic-display-host.service inactive

echo "[v2-acceptance] enter managed domain and present Home"
"$CTL" manager >/dev/null
wait_domain '"domain": "manager"'
wait_unit remagic-display-host.service active
wait_unit remagic-home.service active
wait_unit remagic-runtime.service inactive
home_key=$(foreground_key)
[ "$home_key" = 245209900 ] || fail "unexpected Home surface key: $home_key"
[ "$(display_number panel_failure_count)" = 0 ] || fail "panel submission failed"
remagic_test_u64_nonzero "$(display_number panel_submission_count)" || fail "Home was not submitted"
remagic_test_u64_nonzero "$(display_number visible_signature)" \
    || fail "visible panel buffer has no signature"
assert_presented_surface "$home_key"
home_signature=$(surface_signature "$home_key")
home_generation=$(display_number generation)
home_epoch=$(display_number foreground_epoch)
assert_foreground_submission_since 0 "$home_key" "$home_generation" "$home_epoch" full \
    "initial Home entry"

echo "[v2-acceptance] finger-equivalent tap, press feedback, MagicPaper first frame"
before_full=$(full_refresh_checkpoint)
before_tap_sequence=$(last_submission_sequence)
# Home layout is deterministic: stock, KOReader, then MagicPaper.
"$CTL" tap 200 540 >/dev/null
wait_domain '"foreground": "magicpaper"'
wait_unit 'remagic-app@magicpaper.service' active
magic_key=$(foreground_key)
[ -n "$magic_key" ] && [ "$magic_key" != "$home_key" ] \
    || fail "MagicPaper did not receive foreground surface"
magic_pid=$(main_pid 'remagic-app@magicpaper.service')
[ "$magic_pid" -gt 1 ] || fail "MagicPaper runner has no live PID"
assert_ready_fence magicpaper
assert_presented_surface "$magic_key"
magic_signature=$(surface_signature "$magic_key")
magic_generation=$(display_number generation)
magic_epoch=$(display_number foreground_epoch)
[ "$magic_signature" != "$home_signature" ] \
    || fail "MagicPaper first frame is indistinguishable from Home"
assert_one_full_refresh_since "$before_full" "MagicPaper entry"
assert_home_press_release_since "$before_tap_sequence" "$home_key" "$home_generation" "$home_epoch"
assert_foreground_submission_since "$before_tap_sequence" "$magic_key" "$magic_generation" \
    "$magic_epoch" full "MagicPaper entry"
assert_no_lease_success_after "$MATCHED_SUBMISSION_SEQUENCE" "$home_key" "$home_generation" \
    "$home_epoch" "Home-to-MagicPaper switch"

echo "[v2-acceptance] direct ink path and configured quality-partial idle dissolve"
before_ink_full=$(full_refresh_checkpoint)
before_ink_surface=$(surface_sequence "$magic_key")
before_ink_sequence=$(last_submission_sequence)
"$CTL" pen-line 180 560 700 760 >/dev/null
assert_ink_dissolve_since "$before_ink_sequence" "$before_ink_surface" "$before_ink_full" \
    "$magic_key" "$magic_generation" "$magic_epoch" "MagicPaper direct ink"
[ "$(display_number panel_failure_count)" = 0 ] || fail "direct ink panel submission failed"

echo "[v2-acceptance] park keeps MagicPaper resident"
before_full=$(full_refresh_checkpoint)
before_switch_sequence=$(last_submission_sequence)
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
[ "$(foreground_key)" = "$home_key" ] || fail "Home was not restored after park"
[ "$(main_pid 'remagic-app@magicpaper.service')" = "$magic_pid" ] \
    || fail "MagicPaper was restarted instead of parked"
surface_present "$magic_key" || fail "parked MagicPaper surface disappeared"
wait_panel_settled
[ "$(display_number full_refresh_count)" = "$before_full" ] \
    || fail "parking MagicPaper caused an unnecessary full refresh"
home_generation=$(display_number generation)
home_epoch=$(display_number foreground_epoch)
assert_foreground_submission_since "$before_switch_sequence" "$home_key" "$home_generation" \
    "$home_epoch" content "Home after MagicPaper park"
assert_no_lease_success_after "$MATCHED_SUBMISSION_SEQUENCE" "$magic_key" "$magic_generation" \
    "$magic_epoch" "MagicPaper-to-Home switch"

echo "[v2-acceptance] launch KOReader, park, and resume same process/page"
before_full=$(full_refresh_checkpoint)
before_switch_sequence=$(last_submission_sequence)
"$CTL" launch koreader >/dev/null
wait_domain '"foreground": "koreader"'
wait_unit 'remagic-app@koreader.service' active
assert_freezer_state 'remagic-app@koreader.service' running
koreader_key=$(foreground_key)
koreader_pid=$(main_pid 'remagic-app@koreader.service')
[ "$koreader_pid" -gt 1 ] || fail "KOReader runner has no live PID"
assert_ready_fence koreader
assert_presented_surface "$koreader_key"
koreader_signature=$(surface_signature "$koreader_key")
koreader_generation=$(display_number generation)
koreader_epoch=$(display_number foreground_epoch)
[ "$koreader_signature" != "$home_signature" ] \
    || fail "KOReader first frame is indistinguishable from Home"
[ "$koreader_signature" != "$magic_signature" ] \
    || fail "KOReader first frame is indistinguishable from MagicPaper"
assert_one_full_refresh_since "$before_full" "KOReader entry"
assert_foreground_submission_since "$before_switch_sequence" "$koreader_key" \
    "$koreader_generation" "$koreader_epoch" full "KOReader entry"
assert_no_lease_success_after "$MATCHED_SUBMISSION_SEQUENCE" "$home_key" "$home_generation" \
    "$home_epoch" "Home-to-KOReader switch"
[ "$koreader_key" != "$home_key" ] && [ "$koreader_key" != "$magic_key" ] \
    || fail "KOReader surface lease is not unique"
before_full=$(full_refresh_checkpoint)
before_switch_sequence=$(last_submission_sequence)
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
assert_freezer_state 'remagic-app@koreader.service' frozen
[ "$(main_pid 'remagic-app@koreader.service')" = "$koreader_pid" ] \
    || fail "KOReader was restarted while parking"
surface_present "$koreader_key" || fail "parked KOReader surface disappeared"
wait_panel_settled
[ "$(display_number full_refresh_count)" = "$before_full" ] \
    || fail "parking KOReader caused an unnecessary full refresh"
home_generation=$(display_number generation)
home_epoch=$(display_number foreground_epoch)
assert_foreground_submission_since "$before_switch_sequence" "$home_key" "$home_generation" \
    "$home_epoch" content "Home after KOReader park"
assert_no_lease_success_after "$MATCHED_SUBMISSION_SEQUENCE" "$koreader_key" \
    "$koreader_generation" "$koreader_epoch" "KOReader-to-Home switch"
before_full=$(full_refresh_checkpoint)
before_switch_sequence=$(last_submission_sequence)
"$CTL" launch koreader >/dev/null
wait_domain '"foreground": "koreader"'
assert_freezer_state 'remagic-app@koreader.service' running
[ "$(foreground_key)" = "$koreader_key" ] || fail "KOReader recall used another surface"
[ "$(main_pid 'remagic-app@koreader.service')" = "$koreader_pid" ] \
    || fail "KOReader recall did not resume the same process"
assert_ready_fence koreader
assert_presented_surface "$koreader_key"
assert_one_full_refresh_since "$before_full" "KOReader recall"
koreader_generation=$(display_number generation)
koreader_epoch=$(display_number foreground_epoch)
assert_foreground_submission_since "$before_switch_sequence" "$koreader_key" \
    "$koreader_generation" "$koreader_epoch" full "KOReader recall"
assert_no_lease_success_after "$MATCHED_SUBMISSION_SEQUENCE" "$home_key" "$home_generation" \
    "$home_epoch" "Home-to-KOReader recall"

echo "[v2-acceptance] direct app switch keeps both residents"
before_full=$(full_refresh_checkpoint)
before_switch_sequence=$(last_submission_sequence)
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
assert_freezer_state 'remagic-app@koreader.service' frozen
[ "$(foreground_key)" = "$magic_key" ] || fail "MagicPaper recall surface changed"
[ "$(main_pid 'remagic-app@magicpaper.service')" = "$magic_pid" ] \
    || fail "MagicPaper recall did not resume the same process"
[ "$(main_pid 'remagic-app@koreader.service')" = "$koreader_pid" ] \
    || fail "KOReader was killed during direct switch"
assert_ready_fence magicpaper
assert_presented_surface "$magic_key"
assert_one_full_refresh_since "$before_full" "direct switch to MagicPaper"
magic_generation=$(display_number generation)
magic_epoch=$(display_number foreground_epoch)
assert_foreground_submission_since "$before_switch_sequence" "$magic_key" "$magic_generation" \
    "$magic_epoch" full "direct switch to MagicPaper"
assert_no_lease_success_after "$MATCHED_SUBMISSION_SEQUENCE" "$koreader_key" \
    "$koreader_generation" "$koreader_epoch" "KOReader-to-MagicPaper switch"

echo "[v2-acceptance] close removes task and surface"
before_switch_sequence=$(last_submission_sequence)
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
home_generation=$(display_number generation)
home_epoch=$(display_number foreground_epoch)
assert_foreground_submission_since "$before_switch_sequence" "$home_key" "$home_generation" \
    "$home_epoch" content "Home before close"
assert_no_lease_success_after "$MATCHED_SUBMISSION_SEQUENCE" "$magic_key" "$magic_generation" \
    "$magic_epoch" "MagicPaper-to-Home close switch"
"$CTL" close koreader --complete >/dev/null
wait_unit 'remagic-app@koreader.service' inactive
wait_surface_absent "$koreader_key"
"$CTL" close magicpaper --complete >/dev/null
wait_unit 'remagic-app@magicpaper.service' inactive
wait_surface_absent "$magic_key"
[ "$(systemctl is-active magicpaper-agent.service 2>/dev/null || true)" != active ] \
    || fail "complete MagicPaper close left its background agent running"

echo "[v2-acceptance] serialized return to stock domain"
"$CTL" system >/dev/null
wait_domain '"domain": "system"'
wait_unit remagic-home.service inactive
wait_unit remagic-display-host.service inactive
wait_unit xochitl.service active
set -- /dev/shm/qtfb_*
[ "$1" = '/dev/shm/qtfb_*' ] || fail "QTFB shared surfaces leaked after stock handoff"

echo "[v2-acceptance] PASS"
