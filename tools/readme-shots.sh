#!/bin/bash
# Take the README screenshots (docs/screenshots/*.png) automatically: each
# shot opens Plica on the demo files in a fresh Kitty window, with a Kitty
# theme file copied into a throwaway folder (used both as Kitty's colours and
# as Plica's palette), sets the scene through Kitty's remote-control socket,
# captures that window and closes it. Nothing of yours is used: Kitty runs
# with a temporary config, Plica with temporary config, state and cache.
set -euo pipefail

VERSION=1.1.0
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEMO="$ROOT/docs/screenshots/demo"
OUT="$ROOT/docs/screenshots"
BIN="$ROOT/plica"
themes="${XDG_CONFIG_HOME:-$HOME/.config}/kitty/themes"
only=""
cols=100
rows=30
font=13

usage() {
    cat <<EOF
Usage: tools/readme-shots.sh [options]

Writes the README screenshots to docs/screenshots/. Each one briefly opens
a Kitty window; keep it uncovered until the script says it's done. Needs an
X11 session, kitty and ImageMagick's import (optipng is used if present).

Options:
  -t, --themes DIR   folder of Kitty theme files (default: $themes)
  -o, --only NAME    take just this shot (e.g. python, tables)
  -s, --size CxR     window size in cells (default 100x30)
  -f, --font PT      font size (default 13)
  -h, --help         this help
  -V, --version      print the version
EOF
}

while [[ $# -gt 0 ]]; do
    case $1 in
        -t|--themes) themes=${2:?--themes needs a folder}; shift 2 ;;
        -o|--only) only=${2:?--only needs a shot name}; shift 2 ;;
        -s|--size) cols=${2%x*}; rows=${2#*x}; shift 2 ;;
        -f|--font) font=${2:?--font needs a size}; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        -V|--version) echo "readme-shots.sh $VERSION"; exit 0 ;;
        *) echo "readme-shots.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
done

[[ -x "$BIN" ]] || { echo "readme-shots.sh: $BIN not found; run make build" >&2; exit 1; }
for c in kitty import; do
    command -v $c >/dev/null || { echo "readme-shots.sh: $c not found" >&2; exit 1; }
done
[[ "${XDG_SESSION_TYPE:-x11}" == x11 ]] || echo "readme-shots.sh: warning: import needs X11 (this is $XDG_SESSION_TYPE)" >&2

base=$(mktemp -d -p "${TMPDIR:-/tmp}" plica-shots.XXXXXX)
echo "work folder: $base (left in place)"

# shot NAME THEME_FILE KEYS...  (KEYS: "key:ctrl+f", "text:rain", "sleep:0.5")
shot() {
    local name=$1 theme=$2; shift 2
    [[ -z "$only" || "$only" == "$name" ]] || return 0
    local src="$themes/$theme.conf"
    [[ -f "$src" ]] || { echo "readme-shots.sh: theme $src not found" >&2; return 1; }
    local work="$base/$name"
    # A throwaway HOME for Plica, so paths show as ~/demo/weather.py and
    # ~/.config/plica/config.toml.
    local home="$work/home"
    local files="$home/demo" pal="$home/.config/kitty/themes/${theme##*/}.conf"
    mkdir -p "$files" "$home/.config/plica" "$home/.config/kitty/themes" "$home/.local/state" "$home/.cache"
    cp "$DEMO"/weather.py "$DEMO"/backup.sh "$DEMO"/garden.md "$DEMO"/inventory.txt "$DEMO"/camping.txt "$files/"
    chmod +x "$files/backup.sh" "$files/weather.py"
    cp "$DEMO/config.toml" "$home/.config/plica/config.toml"
    cp "$src" "$pal"
    cat > "$work/kitty.conf" <<EOF
include $pal
font_size $font
initial_window_width ${cols}c
initial_window_height ${rows}c
remember_window_size no
window_padding_width 8
confirm_os_window_close 0
allow_remote_control socket-only
EOF
    local sock="$work/kitty.sock"
    # Kitty resets HOME for its child, so Plica's environment is set inside.
    (cd "$files" && KITTY_CONFIG_DIRECTORY="$work" \
        kitty --config "$work/kitty.conf" --listen-on "unix:$sock" --title "Plica" \
        env -u PLICA_PALETTE -u NO_COLOR HOME="$home" XDG_CONFIG_HOME="$home/.config" \
        XDG_STATE_HOME="$home/.local/state" XDG_CACHE_HOME="$home/.cache" \
        "$BIN" --palette "$pal" weather.py backup.sh garden.md inventory.txt camping.txt) >/dev/null 2>&1 &
    local pid=$!
    for _ in $(seq 60); do [[ -S "$sock" ]] && break; sleep 0.1; done
    sleep 1.2
    local step
    for step in "$@"; do
        case $step in
            key:*) kitty @ --to "unix:$sock" send-key ${step#key:} ;;
            text:*) kitty @ --to "unix:$sock" send-text -- "${step#text:}" ;;
            sleep:*) sleep "${step#sleep:}" ;;
        esac
        sleep 0.15
    done
    sleep 0.6
    local wid
    wid=$(kitty @ --to "unix:$sock" ls | grep -m1 -o '"platform_window_id": [0-9]*' | grep -o '[0-9]*$')
    local png="${outdir:-$OUT}/$name.png"
    import -window "$wid" "$png"
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if command -v optipng >/dev/null; then optipng -quiet -o2 "$png"; fi
    echo "$png ($theme)"
}

# themes: one view of weather.py in four themes, as a 2x2 grid. The four
# tiles stay in the work folder.
themes_grid() {
    [[ -z "$only" || "$only" == themes ]] || return 0
    local saved_only=$only parts=() t i=0
    only=""
    for t in Monokai_Pro faves/everforest_light_soft faves/Alucard faves/GruvboxMaterialLightSoft; do
        i=$((i + 1))
        outdir="$base" cols=76 rows=16 shot "themes-$i" "$t" key:ctrl+home key:ctrl+g text:9 key:enter
        parts+=("$base/themes-$i.png")
    done
    only=$saved_only
    montage "${parts[@]}" -tile 2x2 -geometry +6+6 -background none "$OUT/themes.png"
    if command -v optipng >/dev/null; then optipng -quiet -o2 "$OUT/themes.png"; fi
    echo "$OUT/themes.png"
}

# Plain-text tables (box and ASCII) with highlights; text typed into a cell
# has just widened its column.
rows=33 shot plaintext faves/gruvbox-dark \
    key:alt+5 key:ctrl+home key:ctrl+g text:10 key:enter \
    key:right key:right key:right key:right key:right key:right key:right key:right key:right key:right key:right key:right key:right key:right key:right text:" (butane)"
# Python with syntax colours; the caret on a function name.
shot python faves/Catppuccin-Macchiato \
    key:ctrl+home key:ctrl+g text:22 key:enter key:end
# A Markdown table: the caret in a cell, highlights in the list below.
shot tables faves/gruvbox-dark \
    key:alt+3 key:ctrl+home key:ctrl+g text:11 key:enter key:tab key:tab
# A bordered table with a block of cells selected.
shot bordered Dracula \
    key:alt+4 key:ctrl+home key:ctrl+g text:6 key:enter key:tab \
    key:shift+right key:shift+right key:shift+right key:shift+right key:shift+down key:shift+down key:shift+down
# Find in a light theme: every match marked.
shot find faves/flexoki_light \
    key:alt+2 key:ctrl+f text:backup sleep:0.3
# Preferences over a shell script.
shot preferences OneDark \
    key:alt+2 key:f9 sleep:0.3 key:tab key:tab sleep:0.3
# The same file in four themes.
themes_grid

echo "done"
