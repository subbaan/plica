#!/bin/bash
# Open Plica's demo files (docs/screenshots/demo) in a clean Kitty window,
# ready for screenshots: a built-in theme, a throwaway copy of the files,
# config and state, and Kitty without your config. Nothing of yours (palette,
# highlights, documents, recovery snapshots, key remaps) can show up.
set -euo pipefail

VERSION=1.0.0
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEMO="$ROOT/docs/screenshots/demo"
BIN="$ROOT/plica"

theme=dracula
cols=100
rows=30
font=13
in_place=false

usage() {
    cat <<EOF
Usage: tools/screenshot.sh [options]

Opens Plica on the demo files (weather.py, backup.sh, garden.md,
inventory.txt) in a new Kitty window with a built-in theme. The files,
config and state live in a fresh plica-demo.* folder in \$TMPDIR or /tmp
(left in place; its path is printed), so editing during the shoot changes
nothing else.

Options:
  -t, --theme NAME   dracula (default), gruvbox, nord, solarized-dark,
                     solarized-light
  -s, --size CxR     window size in cells (default 100x30)
  -f, --font PT      font size (default 13)
  -i, --in-place     run in this terminal instead of a new Kitty window
  -h, --help         this help
  -V, --version      print the version
EOF
}

while [[ $# -gt 0 ]]; do
    case $1 in
        -t|--theme) theme=${2:?--theme needs a name}; shift 2 ;;
        -s|--size) cols=${2%x*}; rows=${2#*x}; shift 2 ;;
        -f|--font) font=${2:?--font needs a size}; shift 2 ;;
        -i|--in-place) in_place=true; shift ;;
        -h|--help) usage; exit 0 ;;
        -V|--version) echo "screenshot.sh $VERSION"; exit 0 ;;
        *) echo "screenshot.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
done

# Kitty's own background and text colour around Plica's cells, per theme.
case $theme in
    dracula) bg="#282a36" fg="#f8f8f2" ;;
    gruvbox) bg="#282828" fg="#ebdbb2" ;;
    nord) bg="#2e3440" fg="#d8dee9" ;;
    solarized-dark) bg="#002b36" fg="#839496" ;;
    solarized-light) bg="#fdf6e3" fg="#657b83" ;;
    *) echo "screenshot.sh: unknown theme $theme (see --help)" >&2; exit 2 ;;
esac

[[ -x "$BIN" ]] || { echo "screenshot.sh: $BIN not found; run make build" >&2; exit 1; }

work=$(mktemp -d -p "${TMPDIR:-/tmp}" plica-demo.XXXXXX)
mkdir -p "$work/files" "$work/config/plica" "$work/state" "$work/cache"
cp "$DEMO"/weather.py "$DEMO"/backup.sh "$DEMO"/garden.md "$DEMO"/inventory.txt "$work/files/"
chmod +x "$work/files/backup.sh" "$work/files/weather.py"
cp "$DEMO/config.toml" "$work/config/plica/config.toml"
echo "demo folder: $work"

# Relative names, so the status bar shows "weather.py", not a /tmp path.
cd "$work/files"
files=(weather.py backup.sh garden.md inventory.txt)
run_env=(env -u PLICA_PALETTE -u NO_COLOR
    XDG_CONFIG_HOME="$work/config" XDG_STATE_HOME="$work/state" XDG_CACHE_HOME="$work/cache")

if $in_place; then
    exec "${run_env[@]}" "$BIN" --theme "$theme" "${files[@]}"
fi
command -v kitty >/dev/null || { echo "screenshot.sh: kitty not found; use --in-place" >&2; exit 1; }
"${run_env[@]}" kitty --config NONE --title "Plica" \
    -o font_size="$font" \
    -o initial_window_width="${cols}c" -o initial_window_height="${rows}c" \
    -o remember_window_size=no -o window_padding_width=8 \
    -o background="$bg" -o foreground="$fg" -o cursor="$fg" \
    -o confirm_os_window_close=0 \
    "$BIN" --theme "$theme" "${files[@]}" &
disown
