#!/bin/sh
# Checks the rules that keep BananaSplit small (CI job "Structure"):
#   - core/ (the shared mixer) has no dependencies at all;
#   - each app crate depends only on what its list below allows (the path
#     dependency on core, plus its window and menu bar crates);
#   - each app has its gui_platform.rs (the contract in gui/README.md).
# Prints one line per broken rule and exits 1 if there is any.
#
#   sh scripts/check-structure.sh [repo-root]
#
# Adding a dependency on purpose: add it to the crate's list here in the
# same change, so the review sees it.
set -eu
root=${1:-$(dirname "$0")/..}

# Every dependency of a manifest, one "section<TAB>name<TAB>line" per entry:
# [dependencies], [dev-dependencies], [build-dependencies] and their
# [target.'cfg(...)'.*] forms, and the [dependencies.name] table form.
deps() {
    awk '
        /^[[:space:]]*\[/ {
            sec = $0
            sub(/^[[:space:]]*\[+[[:space:]]*/, "", sec)
            sub(/[[:space:]]*\]+[[:space:]]*(#.*)?$/, "", sec)
            table = ""
            indeps = (sec ~ /(^|\.)(dev-|build-)?dependencies$/)
            # [dependencies.name] / [target.x.dependencies.name]
            if (sec ~ /(^|\.)(dev-|build-)?dependencies\.[^.]+$/) {
                table = sec
                sub(/.*dependencies\./, "", table)
                gsub(/"/, "", table)
                print sec "\t" table "\t" NR
            }
            next
        }
        indeps && /^[[:space:]]*[A-Za-z0-9_"-]+[[:space:]]*=/ {
            name = $0
            sub(/[[:space:]]*=.*/, "", name)
            gsub(/[[:space:]"]/, "", name)
            print sec "\t" name "\t" NR "\t" $0
        }
    ' "$1"
}

fail=0
bad() {
    echo "structure: $*"
    fail=1
}

# (a) core: nothing at all
core="$root/core/Cargo.toml"
if [ ! -f "$core" ]; then
    bad "core/Cargo.toml is missing"
else
    deps "$core" | while IFS="$(printf '\t')" read -r sec name line _; do
        echo "structure: core/Cargo.toml:$line: [$sec] has \"$name\": the shared core must have no dependencies"
    done | grep . && fail=1
fi

# (b) the apps: only their allowed names; core only as the path dependency
check_app() { # crate, allowed names (space-separated)
    m="$root/$1/Cargo.toml"
    if [ ! -f "$m" ]; then
        bad "$1/Cargo.toml is missing"
        return
    fi
    out=$(deps "$m" | while IFS="$(printf '\t')" read -r sec name line text; do
        case " $2 " in
            *" $name "*) ;;
            *)
                echo "structure: $1/Cargo.toml:$line: [$sec] has \"$name\", which is not in $1's allowed list ($2) in scripts/check-structure.sh"
                continue
                ;;
        esac
        if [ "$name" = bananasplit-core ]; then
            case "$text" in
                *'path = "../core"'*) ;;
                *) echo "structure: $1/Cargo.toml:$line: bananasplit-core must be the path dependency on ../core" ;;
            esac
        fi
    done)
    if [ -n "$out" ]; then
        echo "$out"
        fail=1
    fi
}
check_app linux "bananasplit-core"
check_app windows "bananasplit-core tao wry"
check_app macos "bananasplit-core tao wry tray-icon"

# (c) every app has its side of the GUI contract
for app in macos linux windows; do
    [ -f "$root/$app/src/gui_platform.rs" ] ||
        bad "$app/src/gui_platform.rs is missing (every app provides it: gui/README.md)"
done

if [ "$fail" -eq 0 ]; then
    echo "structure: ok (core has no dependencies; linux, windows and macos only their allowed ones; every gui_platform.rs is there)"
fi
exit "$fail"
