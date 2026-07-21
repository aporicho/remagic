#!/usr/bin/env bash
set -euo pipefail

# Keep source units small enough for focused human and AI review. Tests may be
# larger because tables and fixtures are intentionally colocated; generated,
# vendored, and patch payloads are reviewed at their source instead.
readonly TARGET_LINES=400
readonly MAX_SOURCE_LINES=500
readonly MAX_TEST_LINES=800
readonly TARGET_FUNCTION_LINES=60
readonly MAX_FUNCTION_LINES=100
readonly EXCEPTION_FILE=architecture-exceptions.tsv

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

failures=0
warnings=0

validate_exceptions() {
    [[ -f "$EXCEPTION_FILE" ]] || return
    local kind path limit reason
    while IFS=$'\t' read -r kind path limit reason; do
        [[ -z "$kind" || "$kind" == \#* ]] && continue
        if [[ "$kind" != file || -z "$path" || ! "$limit" =~ ^[0-9]+$ || ${#reason} -lt 12 ]]; then
            printf 'ARCH ERROR: invalid exception row: %s | %s | %s | %s\n' "$kind" "$path" "$limit" "$reason" >&2
            failures=$((failures + 1))
            continue
        fi
        if [[ "$path" == *'*'* || "$path" == */ || ! -f "$path" ]]; then
            printf 'ARCH ERROR: exception must name one existing file: %s\n' "$path" >&2
            failures=$((failures + 1))
        fi
    done < "$EXCEPTION_FILE"
}

exception_limit() {
    local path=$1
    [[ -f "$EXCEPTION_FILE" ]] || return 1
    awk -F '\t' -v path="$path" '$1 == "file" && $2 == path { print $3; found = 1; exit } END { if (!found) exit 1 }' "$EXCEPTION_FILE"
}

is_test_file() {
    case "$1" in
        tests/*|test/*|tests.rs|test_*.rs|*/tests/*|*/test/*|*/tests.rs|*_test.rs|*/test_*.rs) return 0 ;;
        *) return 1 ;;
    esac
}

check_file_size() {
    local path=$1 lines limit exception=""
    lines=$(wc -l < "$path")
    if exception=$(exception_limit "$path"); then
        limit=$exception
    elif is_test_file "$path"; then
        limit=$MAX_TEST_LINES
    else
        limit=$MAX_SOURCE_LINES
    fi
    if (( lines > limit )); then
        printf 'ARCH ERROR: %s has %d lines (limit %d)\n' "$path" "$lines" "$limit" >&2
        failures=$((failures + 1))
    elif [[ -n "$exception" ]] && (( lines > TARGET_LINES )); then
        printf 'ARCH NOTE:  %s uses its documented %d-line exception (%d lines)\n' "$path" "$limit" "$lines" >&2
    elif ! is_test_file "$path" && (( lines > TARGET_LINES )); then
        printf 'ARCH WARN:  %s has %d lines (target %d)\n' "$path" "$lines" "$TARGET_LINES" >&2
        warnings=$((warnings + 1))
    fi
}

check_rust_functions() {
    local path=$1
    if is_test_file "$path"; then
        return
    fi
    if ! awk -v file="$path" -v target="$TARGET_FUNCTION_LINES" -v maximum="$MAX_FUNCTION_LINES" '
        function finish(end_line, size) {
            if (!active) return
            size = end_line - start + 1
            if (size > maximum) {
                printf "ARCH ERROR: %s:%d function %s has %d lines (limit %d)\n", file, start, name, size, maximum > "/dev/stderr"
                bad = 1
            } else if (size > target) {
                printf "ARCH WARN:  %s:%d function %s has %d lines (target %d)\n", file, start, name, size, target > "/dev/stderr"
            }
            active = 0; seen_body = 0; depth = 0
        }
        /^[[:space:]]*((pub\([^)]*\)|pub)[[:space:]]+)?((async|const|unsafe|extern)[[:space:]]+)*fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*/ {
            if (active) finish(NR - 1)
            active = 1; start = NR; depth = 0; seen_body = 0
            name = $0
            sub(/^[[:space:]]*((pub\([^)]*\)|pub)[[:space:]]+)?((async|const|unsafe|extern)[[:space:]]+)*fn[[:space:]]+/, "", name)
            sub(/[^A-Za-z0-9_].*$/, "", name)
        }
        active {
            line = $0
            sub(/\/\/.*/, "", line)
            opens = gsub(/\{/, "{", line)
            closes = gsub(/\}/, "}", line)
            if (opens > 0) seen_body = 1
            depth += opens - closes
            if (!seen_body && line ~ /;/) active = 0
            else if (seen_body && depth <= 0) finish(NR)
        }
        END { if (active) finish(NR); exit bad }
    ' "$path"; then
        failures=$((failures + 1))
    fi
}

validate_exceptions

while IFS= read -r -d '' path; do
    path=${path#./}
    check_file_size "$path"
    case "$path" in
        *.rs) check_rust_functions "$path" ;;
    esac
done < <(
    find . \
        \( -path './.git' -o -path './target' -o -path '*/target' \
           -o -path './dist' -o -path './vendor' -o -path './third_party' \) -prune -o \
        -type f \( -name '*.rs' -o -name '*.c' -o -name '*.h' \
                     -o -name '*.lua' -o -name '*.sh' \
                     -o \( -path './scripts/*' ! -name '*.*' \) \) \
        ! -name '*.patch' -print0
)

if (( failures > 0 )); then
    printf 'Architecture check failed: %d violation(s).\n' "$failures" >&2
    exit 1
fi
printf 'Architecture check passed; review warnings and documented exceptions above.\n'
