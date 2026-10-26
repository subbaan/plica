# Releasing Plica

Steps for a public release. Nothing here runs automatically, and publishing is always a deliberate, manual step.

## Once, before the first public release

1. **Licence:** MIT (`LICENSE`, `license = "MIT"` in `Cargo.toml`). Done in 0.5.5.
2. **Donations (optional):** add `.github/FUNDING.yml`, for example `github: subbaan` or `ko_fi: name`, so GitHub shows a Sponsor button, and mention it in the README.
3. **Repository visibility:** make `github.com/subbaan/plica` public.

4. **Name check** (done 2026-10-23, not legal advice): no software package called `plica` on crates.io, the AUR, PyPI, npm or Homebrew, and none in any distribution Repology tracks, so `/usr/bin/plica` is free. On GitHub, two tiny unrelated repositories share the name (`Akamig/plica`, 1 star; `theagitist/plica`, a PDF imposition tool, 1 star). Web searches for "plica text editor" find nothing. "Plica" is also an anatomical term (plica syndrome), so search results for the bare word are medical. Searching "plica editor" works. The crates.io name is unclaimed; publishing there (`cargo publish`) would reserve it, and that is the user's decision.

## Every release

1. Version bumped in `Cargo.toml` and in `docs/plica.1` (`.TH` line), with a `CHANGELOG.md` entry.
2. `make check && make e2e && make perf`, plus the manual checklists in `docs/` as relevant.
3. `make dist` → `dist/plica-VERSION-x86_64-linux.tar.gz` and its `.sha256`. It runs `make check` and regenerates `THIRD_PARTY_LICENSES.md`; commit that file if it changed.
4. Tag and publish (the commit is already pushed):

   ```sh
   git tag -a vVERSION -m "Plica VERSION"
   git push origin vVERSION
   gh release create vVERSION dist/plica-VERSION-x86_64-linux.tar.gz dist/plica-VERSION-x86_64-linux.tar.gz.sha256 \
       --title "Plica VERSION" --notes-file <(sed -n '/^## VERSION /,/^## /p' CHANGELOG.md | sed '$d')
   ```

## Screenshots

`tools/readme-shots.sh` retakes the README screenshots (`docs/screenshots/*.png`) automatically: each shot opens a throwaway Kitty with a copied theme from `~/.config/kitty/themes` (`--themes DIR` for another folder), sets the scene with Kitty's remote control and captures only that window. `--only NAME` retakes one. It needs X11, kitty and ImageMagick. Retake them before a release if the UI changed.

`tools/screenshot.sh` (`-t nord`, `-t solarized-light`, `-s 120x34`, `--help`) opens the demo files in a new Kitty window with a built-in theme and nothing from your own setup. Tabs: `weather.py`, `backup.sh`, `garden.md` (a Markdown table and highlights) and `inventory.txt` (a box table). Ctrl+PageDown moves between them.

## Portability of the binary

`make dist` packages a **static musl build** (`target/x86_64-unknown-linux-musl/release/plica`), which runs on any x86-64 Linux whatever its glibc. It needs the musl target (`sudo pacman -S rust-musl` on Arch/Manjaro, or `rustup target add x86_64-unknown-linux-musl`). Measured against the glibc build: the same on the 35 MB perf test, about 30% slower only for typing that widens a column of a 50 000-row table (musl's allocator). The everyday `./plica` stays the glibc build.
