#!/bin/sh
# Cross-builds dist/koeboard-windows-x64.zip on macOS.
# Needs: brew install mingw-w64 && rustup target add x86_64-pc-windows-gnu
set -eu
cd "$(dirname "$0")/.."
target=x86_64-pc-windows-gnu
cargo build --release --target "$target"
out=dist/koeboard-windows-x64
rm -rf "$out" "$out.zip"
mkdir -p "$out"
release="target/$target/release"
cp "$release/koeboard.exe" "$release/sherpa-onnx-c-api.dll" "$release/onnxruntime.dll" \
   "$release/onnxruntime_providers_shared.dll" README.md THIRD_PARTY.md "$out/"
cp "$(find "$release/build" -path '*webview2-com-sys*/out/x64/WebView2Loader.dll' | head -1)" "$out/"
cp assets/fonts/OFL.txt "$out/OFL-NotoSansJP.txt"
/usr/bin/python3 -I scripts/generate-licenses.py "$target" "$out/THIRD_PARTY_LICENSES.txt"
(cd dist && zip -qr koeboard-windows-x64.zip koeboard-windows-x64)
echo "$out.zip"
