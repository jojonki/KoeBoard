#!/bin/sh
# Regenerates AppIcon.icns and koeboard.ico from assets/icons/app-icon.png.
# Needs: macOS (iconutil) and ImageMagick.
set -eu
cd "$(dirname "$0")/../assets/icons"
src=app-icon.png
set_dir=$(mktemp -d)/AppIcon.iconset
mkdir -p "$set_dir"
# macOS icons keep a margin: the body is 824/1024 of the canvas.
rep() {
    body=$(($2 * 824 / 1024))
    magick "$src" -filter Lanczos -resize "${body}x${body}" -background none \
        -gravity center -extent "$2x$2" "PNG32:$set_dir/$1.png"
}
rep icon_16x16 16
rep icon_16x16@2x 32
rep icon_32x32 32
rep icon_32x32@2x 64
rep icon_128x128 128
rep icon_128x128@2x 256
rep icon_256x256 256
iconutil -c icns -o AppIcon.icns "$set_dir"
magick "$src" -define icon:auto-resize=256,64,48,32,24,16 koeboard.ico
