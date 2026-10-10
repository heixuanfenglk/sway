#!/usr/bin/env bash
# Build Sway release and create a macOS .app + .dmg installer.
#
# Steps:
#   1. cargo build --release
#   2. Build Sway.app (Info.plist + icon.icns)
#   3. Create compressed DMG under dist/
#
# Usage:
#   ./packaging/build-installer.sh
#   ./packaging/build-installer.sh --skip-build
#   ./packaging/build-installer.sh --no-dmg          # only produce .app
#   ./packaging/build-installer.sh --sign            # ad-hoc codesign
#
set -euo pipefail

SKIP_BUILD=0
MAKE_DMG=1
ADHOC_SIGN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build|-s) SKIP_BUILD=1; shift ;;
    --no-dmg)        MAKE_DMG=0; shift ;;
    --sign)          ADHOC_SIGN=1; shift ;;
    -h|--help)
      sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RELEASE_BIN="$REPO_ROOT/target/release/sway"
DIST_DIR="$REPO_ROOT/dist"
ICON_PNG="$REPO_ROOT/assets/icon.png"
ICON_FALLBACK="$REPO_ROOT/assets/icon-256.png"

APP_NAME="Sway"
APP_NAME_FULL="Sway · 全局代理"
BUNDLE_ID="com.sway.globalproxy"
EXECUTABLE_NAME="Sway"

cd "$REPO_ROOT"

get_app_version() {
  local ver
  ver="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
  if [[ -z "$ver" ]]; then
    echo "Cannot read version from Cargo.toml" >&2
    exit 1
  fi
  printf '%s' "$ver"
}

host_arch() {
  case "$(uname -m)" in
    arm64|aarch64) echo "arm64" ;;
    x86_64)        echo "x64" ;;
    *)             uname -m ;;
  esac
}

build_icns() {
  local src_png="$1"
  local out_icns="$2"
  local iconset
  iconset="$(mktemp -d)/AppIcon.iconset"

  mkdir -p "$iconset"

  # Prefer a high-res source; fall back to 256 if needed.
  if [[ ! -f "$src_png" ]]; then
    src_png="$ICON_FALLBACK"
  fi
  if [[ ! -f "$src_png" ]]; then
    echo "No icon PNG found under assets/; skipping .icns" >&2
    rm -rf "$(dirname "$iconset")"
    return 1
  fi

  local size
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$src_png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  done
  # @2x variants
  sips -z 32 32 "$src_png" --out "$iconset/icon_16x16@2x.png" >/dev/null
  sips -z 64 64 "$src_png" --out "$iconset/icon_32x32@2x.png" >/dev/null
  sips -z 256 256 "$src_png" --out "$iconset/icon_128x128@2x.png" >/dev/null
  sips -z 512 512 "$src_png" --out "$iconset/icon_256x256@2x.png" >/dev/null
  sips -z 1024 1024 "$src_png" --out "$iconset/icon_512x512@2x.png" >/dev/null

  iconutil -c icns "$iconset" -o "$out_icns"
  rm -rf "$(dirname "$iconset")"
}

write_info_plist() {
  local plist_path="$1"
  local version="$2"
  cat > "$plist_path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>zh-Hans</string>
	<key>CFBundleDisplayName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleExecutable</key>
	<string>${EXECUTABLE_NAME}</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleIdentifier</key>
	<string>${BUNDLE_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>${APP_NAME}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>${version}</string>
	<key>CFBundleVersion</key>
	<string>${version}</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>Copyright © Sway. ${APP_NAME_FULL}</string>
</dict>
</plist>
EOF
}

create_app_bundle() {
  local version="$1"
  local app_dir="$2"
  local macos_dir="$app_dir/Contents/MacOS"
  local resources_dir="$app_dir/Contents/Resources"

  rm -rf "$app_dir"
  mkdir -p "$macos_dir" "$resources_dir"

  cp "$RELEASE_BIN" "$macos_dir/$EXECUTABLE_NAME"
  chmod +x "$macos_dir/$EXECUTABLE_NAME"

  write_info_plist "$app_dir/Contents/Info.plist" "$version"

  if build_icns "$ICON_PNG" "$resources_dir/AppIcon.icns"; then
    echo "==> icon: $resources_dir/AppIcon.icns" >&2
  else
    # Remove icon key reference if icns failed — avoid broken Finder icon
    /usr/libexec/PlistBuddy -c "Delete :CFBundleIconFile" "$app_dir/Contents/Info.plist" 2>/dev/null || true
  fi

  # Clear quarantine so local builds open without Gatekeeper friction
  xattr -cr "$app_dir" 2>/dev/null || true

  if [[ "$ADHOC_SIGN" -eq 1 ]]; then
    echo "==> ad-hoc codesign..." >&2
    codesign --force --deep --sign - "$app_dir"
  fi
}

create_dmg() {
  local version="$1"
  local app_dir="$2"
  local arch
  arch="$(host_arch)"
  local dmg_name="${APP_NAME}-${version}-macos-${arch}.dmg"
  local dmg_path="$DIST_DIR/$dmg_name"
  local stage
  stage="$(mktemp -d)/dmg"
  local volume_name="${APP_NAME} ${version}"

  mkdir -p "$stage"
  cp -R "$app_dir" "$stage/"
  ln -s /Applications "$stage/Applications"

  rm -f "$dmg_path"
  echo "==> creating DMG: $dmg_path" >&2
  hdiutil create \
    -volname "$volume_name" \
    -srcfolder "$stage" \
    -ov \
    -format UDZO \
    -fs HFS+ \
    "$dmg_path" >/dev/null

  rm -rf "$(dirname "$stage")"
  xattr -cr "$dmg_path" 2>/dev/null || true

  if [[ ! -f "$dmg_path" ]]; then
    echo "DMG not created: $dmg_path" >&2
    exit 1
  fi
  printf '%s' "$dmg_path"
}

# --- main -------------------------------------------------------------------

VERSION="$(get_app_version)"
ARCH="$(host_arch)"
APP_DIR="$DIST_DIR/${APP_NAME}.app"

echo "==> ${APP_NAME} ${VERSION} (${ARCH}) - building macOS package"

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  echo "==> cargo build --release"
  cargo build --release
else
  echo "==> skip build (--skip-build)"
fi

if [[ ! -f "$RELEASE_BIN" ]]; then
  echo "Executable not found: $RELEASE_BIN" >&2
  echo "Run cargo build --release first, or omit --skip-build." >&2
  exit 1
fi

mkdir -p "$DIST_DIR"

echo "==> assembling ${APP_NAME}.app..."
create_app_bundle "$VERSION" "$APP_DIR"

echo ""
echo "App:  $APP_DIR"

if [[ "$MAKE_DMG" -eq 1 ]]; then
  DMG_PATH="$(create_dmg "$VERSION" "$APP_DIR")"
  SIZE_MB="$(du -h "$DMG_PATH" | awk '{print $1}')"
  echo "DMG:  $DMG_PATH ($SIZE_MB)"
fi

echo ""
echo "Done."
echo "Install tip: open the DMG and drag ${APP_NAME}.app into Applications."
echo "If Gatekeeper blocks launch:  xattr -cr \"/Applications/${APP_NAME}.app\""
