#!/usr/bin/env bash
# Puts this checkout's rekcod on your PATH: a symlink $DIR/rekcod -> target/release/rekcod.
# Once per clone; every scripts/build.sh after that is picked up without relinking. rekcod
# resolves the link, so it still finds out/guest next to the checkout.
#
# DIR=~/bin scripts/link.sh   link there instead of ~/.local/bin
# scripts/link.sh --remove    remove the link (only if it points into this checkout)
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
dir=${DIR:-$HOME/.local/bin}
link=$dir/rekcod
target=$repo/target/release/rekcod

points_here() { [[ -L $link && $(readlink "$link") == "$target" ]]; }

if [[ ${1:-} == --remove ]]; then
  points_here || { echo "rekcod: $link isn't a link to $target; leaving it" >&2; exit 1; }
  rm "$link" && echo "rekcod: removed $link"
  exit 0
fi

# A real binary there is a release install (scripts/install.sh, a pkg); don't clobber it.
if [[ -e $link && ! -L $link ]]; then
  echo "rekcod: $link is a file, not a link; move it away first" >&2
  exit 1
fi
[[ -x $target ]] || echo "rekcod: $target isn't built yet; run scripts/build.sh --host-only"
mkdir -p "$dir"
ln -sfn "$target" "$link"
echo "rekcod: $link -> $target"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "rekcod: $dir isn't on your PATH; add it: echo 'export PATH=\"$dir:\$PATH\"' >> ~/.zshrc" ;;
esac
# An earlier rekcod on PATH would win over the link.
first=$(command -v rekcod 2>/dev/null || true)
if [[ -n $first && $first != "$link" ]]; then
  echo "rekcod: $first comes first on your PATH and shadows the link"
fi
