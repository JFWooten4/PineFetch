#!/usr/bin/env bash
set -euo pipefail

# Build-time only. Resource names keep architecture-specific helpers separate.
OCR_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OCR_RUNTIME="$OCR_ROOT/src-tauri/resources/ocr-runtime"
if [[ "$(uname -s)" != Darwin ]]; then
  exit 0
fi
OCR_MIN_VERSION="${MACOSX_DEPLOYMENT_TARGET:-10.13}"
OCR_ARCH="${PINEFETCH_OCR_TARGET_ARCH:-$(uname -m)}"
case "$OCR_ARCH" in
  aarch64|arm64) OCR_ARCH=arm64; if [[ "${OCR_MIN_VERSION%%.*}" -lt 11 ]]; then OCR_MIN_VERSION=11.0; fi ;;
  x86_64) ;;
  *) echo "Unsupported Apple Vision helper architecture: $OCR_ARCH" >&2; exit 1 ;;
esac
OCR_SOURCE="$OCR_ROOT/native/vision-ocr.swift"
OCR_HELPER="$OCR_RUNTIME/vision-ocr-$OCR_ARCH"
OCR_STAMP="$OCR_RUNTIME/vision-ocr-$OCR_ARCH.build"
OCR_SDK="${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}"
OCR_BUILD_KEY="$({
  shasum -a 256 "$OCR_SOURCE" "${BASH_SOURCE[0]}" | awk '{print $1}'
  printf '%s\n' "$OCR_ARCH" "$OCR_MIN_VERSION" "$OCR_SDK"
  if [[ -f "$OCR_SDK/SDKSettings.json" ]]; then shasum -a 256 "$OCR_SDK/SDKSettings.json" | awk '{print $1}'; fi
  xcrun swiftc --version
} | shasum -a 256 | awk '{print $1}')"
if [[ -x "$OCR_HELPER" && -f "$OCR_STAMP" ]] &&
   read -r OCR_CACHED_KEY OCR_CACHED_HASH < "$OCR_STAMP" &&
   [[ "$OCR_CACHED_KEY" == "$OCR_BUILD_KEY" ]] &&
   [[ "$OCR_CACHED_HASH" == "$(shasum -a 256 "$OCR_HELPER" | awk '{print $1}')" ]]; then
  exit 0
fi

OCR_WORK="$(mktemp -d "${TMPDIR:-/tmp}/pinefetch-vision-build.XXXXXX")"
trap 'rm -rf "$OCR_WORK"' EXIT
mkdir -p "$OCR_RUNTIME"
xcrun swiftc -O -swift-version 5 -target "$OCR_ARCH-apple-macosx$OCR_MIN_VERSION" \
  -sdk "$OCR_SDK" "$OCR_SOURCE" -o "$OCR_WORK/vision-ocr"
codesign --force --sign - "$OCR_WORK/vision-ocr"
printf '%s %s\n' "$OCR_BUILD_KEY" "$(shasum -a 256 "$OCR_WORK/vision-ocr" | awk '{print $1}')" > "$OCR_WORK/build"
if [[ ! -x "$OCR_HELPER" ]] || ! cmp -s "$OCR_WORK/vision-ocr" "$OCR_HELPER"; then
  mv "$OCR_WORK/vision-ocr" "$OCR_HELPER"
fi
mv "$OCR_WORK/build" "$OCR_STAMP"
