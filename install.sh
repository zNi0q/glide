#!/bin/sh
set -eu

repo="zNi0q/glide"
bin_dir="$HOME/.local/bin"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"

case "$(uname -m)" in
  x86_64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *)
    echo "glide has no release for $(uname -m) machines" >&2
    exit 1
    ;;
esac

tag=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")
tag=${tag##*/}
name="glide-$tag-$arch-linux"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cd "$work"
echo "Downloading glide $tag for $arch"
curl -fsSLO "https://github.com/$repo/releases/download/$tag/$name.tar.gz"
curl -fsSLO "https://github.com/$repo/releases/download/$tag/$name.tar.gz.sha256"
sha256sum -c "$name.tar.gz.sha256" > /dev/null
tar -xzf "$name.tar.gz"

install -Dm755 "$name/glide" "$bin_dir/glide"
install -Dm644 "$name/logo.svg" "$data_dir/icons/hicolor/scalable/apps/glide.svg"
sed "s|^Exec=glide |Exec=$bin_dir/glide |" "$name/glide.desktop" > glide.desktop
install -Dm644 glide.desktop "$data_dir/applications/glide.desktop"
if command -v update-desktop-database > /dev/null; then
  update-desktop-database "$data_dir/applications" || true
fi

echo "Installed glide $tag to $bin_dir/glide"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add $bin_dir to your PATH to run glide from a terminal." ;;
esac
