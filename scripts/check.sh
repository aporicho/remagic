#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

scripts/check-architecture.sh
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --manifest-path native/remagic-display-host/Cargo.toml --all-targets
cargo clippy --manifest-path native/remagic-display-host/Cargo.toml --all-targets -- -D warnings

while IFS= read -r -d '' script; do
    if [[ "$script" == *.sh ]]; then
        bash -n "$script"
    else
        case "$(head -n 1 "$script")" in
            '#!'*sh*) bash -n "$script" ;;
        esac
    fi
done < <(find scripts native/appload-runtime -type f -print0)
sh tests/test-deployment-safety.sh
sh tests/test-koreader-adapter-inventory.sh
sh tests/test-lock-acceptance-inventory.sh
sh tests/test-device-test-isolation.sh
sh tests/test-magicpaper-config-isolation.sh
sh tests/test-magicpaper-data-migrate.sh
sh tests/test-system-release.sh
sh tests/test-dual-usb-tools.sh

echo "remagic checks passed"
