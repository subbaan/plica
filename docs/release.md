# Releasing Plica

Steps for a public release. Nothing here runs automatically, and publishing is always a deliberate, manual step.

## Once, before the first public release

1. **Licence:** add a `LICENSE` file (for example MIT, `MIT OR Apache-2.0`, or GPL-3.0-or-later) and set `license = "…"` in `Cargo.toml`. `make dist` includes `LICENSE` automatically once it exists.
2. **Donations (optional):** add `.github/FUNDING.yml`, for example `github: subbaan` or `ko_fi: name`, so GitHub shows a Sponsor button, and mention it in the README.
3. **Repository visibility:** make `github.com/subbaan/plica` public.

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

## Portability of the binary

The binary is built against the build machine's glibc: currently glibc **2.39**, so it runs on Arch/Manjaro, Fedora 40+, Ubuntu 24.04+ and Debian 13. It does not run on Debian 12 or Ubuntu 22.04. For a binary that runs on any x86-64 Linux, build statically with musl:

```sh
# Arch/Manjaro: sudo pacman -S rust-musl     (or: rustup target add x86_64-unknown-linux-musl)
cargo build --release --target x86_64-unknown-linux-musl
```

Building on an older distribution, for example in a container, also works. Users can always build from source with `make build`.
