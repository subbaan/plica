#!/bin/bash
# Nightly backup of the projects folder, keeping the last 7 archives.
set -euo pipefail

src="${1:-$HOME/projects}"
dest="/mnt/backup"
stamp=$(date +%Y-%m-%d)

if [[ ! -d "$dest" ]]; then
    echo "backup drive not mounted" >&2
    exit 1
fi

tar -czf "$dest/projects-$stamp.tar.gz" -C "$src" .
# FIXME: check free space first
ls -1t "$dest"/projects-*.tar.gz | tail -n +8 | xargs -r rm --

echo "done: projects-$stamp.tar.gz ($(du -h "$dest/projects-$stamp.tar.gz" | cut -f1))"
