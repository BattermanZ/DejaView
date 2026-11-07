# DejaView's checks and live runs. Run `just` with no argument to list them.
#
# CI runs `just check` and `just test`, so the gate is defined once, here.

set positional-arguments := true

default:
    @just --list

# Dev state: the sandbox tree and the config, log and cache it uses. Disposable.
dev_dir := ".dev"
sandbox := dev_dir + "/sandbox"

# The cargo target directory: respect CARGO_TARGET_DIR where a host sets one.
target_dir := env_var_or_default("CARGO_TARGET_DIR", "target")

# Formatting and lints, changing nothing. Warnings are failures.
check:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

# As root, five engine tests return early and still report ok, because root
# can read and write the files they make unreadable. So under root the unit
# tests run a second time as `nobody`, where they really run.
#
# Every test, and under root the unit tests again as nobody.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo test --locked
    if [ "$(id -u)" = 0 ]; then
        exe="$(cargo test --locked --bin dejaview --no-run 2>&1 \
            | sed -n 's/.*unittests src\/main\.rs (\(.*\))$/\1/p')"
        [ -n "$exe" ] || { echo "error: could not find the unit test binary" >&2; exit 1; }
        case "$exe" in /*) ;; *) exe="$PWD/$exe" ;; esac
        echo "── Unit tests again as nobody: root skips five of them ──"
        su -s /bin/sh nobody -c "cd /tmp && '$exe'"
    fi

# Apply what `just check` reports about formatting.
format:
    cargo fmt

# Run it after changing src/engine/hashing.rs, cache.rs or collect.rs.
#
# The perf harness, in a release build.
perf:
    cargo test --release --locked --test perf_harness -- --ignored --nocapture

# A downloads folder and a media folder under .dev/sandbox, of sparse files
# with film and episode sizes (see scripts/sandbox.sh for what each one
# shows): rows that become hardlinks, rows that become symlinks because their
# library file is on a second filesystem, rows already linked, files that
# differ, and a link and a read that fail. It starts over each time, and
# points the config `just try` uses at itself.
#
# A fresh scratch library to run DejaView against.
sandbox:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf "{{ dev_dir }}/home"
    # The second disk lives in memory, under a name unique to this checkout.
    shm=""
    # It needs a private mount namespace, which some containers refuse.
    if [ -d /dev/shm ] && command -v setpriv >/dev/null \
        && unshare --user --map-root-user --mount true 2>/dev/null; then
        shm="/dev/shm/dejaview-sandbox-$(printf '%s' "$PWD" | cksum | cut -d' ' -f1)"
    fi
    scripts/sandbox.sh "$PWD/{{ sandbox }}" "$shm"
    just try config set-downloads "$PWD/{{ sandbox }}/downloads" >/dev/null
    just try config set-media "$PWD/{{ sandbox }}/media" >/dev/null
    just try config set-min-age 0 >/dev/null
    echo "Sandbox ready under {{ sandbox }}. Try: just try tui"

# Its config and log are kept under .dev/home, so the real ones are never
# touched. The cache lives in the downloads folder, as always. When the
# sandbox has a second disk, dejaview runs in a private mount namespace where
# that disk is mounted into the library, as a user that holds no
# capabilities, so file permissions still apply.
#
# Run dejaview against the sandbox: `just try tui`, `just try scan`, `just try link --json`.
try *args:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked --quiet
    home="$PWD/{{ dev_dir }}/home"
    mkdir -p "$home"
    run=(env HOME="$home" XDG_CONFIG_HOME="$home/config" XDG_STATE_HOME="$home/state" RUST_BACKTRACE=0
        "{{ target_dir }}/debug/dejaview" "$@")
    mounts="{{ sandbox }}/.second-disk"
    if [ ! -f "$mounts" ]; then
        exec "${run[@]}"
    fi
    while IFS=$'\t' read -r src _; do
        [ -d "$src" ] || { echo "error: the sandbox's second disk is gone (after a restart?). Run: just sandbox" >&2; exit 1; }
    done < "$mounts"
    exec unshare --user --map-root-user --mount -- bash -c '
        set -euo pipefail
        while IFS=$'"'"'\t'"'"' read -r src dst; do mount --bind "$src" "$dst"; done < "$1"
        shift
        exec setpriv --inh-caps=-all --bounding-set=-all -- "$@"
    ' _ "$mounts" "${run[@]}"
