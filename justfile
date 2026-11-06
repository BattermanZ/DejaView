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

# A downloads folder and a media folder under .dev/sandbox, with duplicate
# pairs, a same-size file that differs, and two copies of one library file in
# the downloads folder. It starts over each time, and points the config `just
# try` uses at itself.
#
# A fresh scratch library to run DejaView against.
sandbox:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf "{{ sandbox }}" "{{ dev_dir }}/home"
    d="{{ sandbox }}/downloads"; m="{{ sandbox }}/media"
    mkdir -p "$d/Arrival.2016.1080p" "$d/Show.S01" "$m/Movies" "$m/TV/Show/Season 1"
    fill() { head -c "$2" /dev/urandom > "$1"; }
    fill "$m/Movies/Arrival (2016).mkv" 3000000
    fill "$m/TV/Show/Season 1/Show S01E01.mkv" 2000000
    fill "$m/TV/Show/Season 1/Show S01E02.mkv" 2000000
    cp "$m/Movies/Arrival (2016).mkv" "$d/Arrival.2016.1080p/Arrival.2016.1080p.mkv"
    cp "$m/TV/Show/Season 1/Show S01E01.mkv" "$d/Show.S01/Show.S01E01.mkv"
    cp "$m/TV/Show/Season 1/Show S01E01.mkv" "$d/Show.S01/Show.S01E01.copy.mkv"
    fill "$d/Show.S01/Show.S01E02.mkv" 2000000
    just try config set-downloads "$PWD/$d" >/dev/null
    just try config set-media "$PWD/$m" >/dev/null
    just try config set-min-age 0 >/dev/null
    echo "Sandbox ready under {{ sandbox }}: 3 duplicates of 2 library files, 1 same-size file that differs."
    echo "Try: just try scan"

# Its config and log are kept under .dev/home, so the real ones are never
# touched. The cache lives in the downloads folder, as always.
#
# Run dejaview against the sandbox: `just try scan`, `just try link --json`.
try *args:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked --quiet
    home="$PWD/{{ dev_dir }}/home"
    mkdir -p "$home"
    HOME="$home" XDG_CONFIG_HOME="$home/config" XDG_STATE_HOME="$home/state" RUST_BACKTRACE=0 \
        "{{ target_dir }}/debug/dejaview" "$@"
