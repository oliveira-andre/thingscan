#!/bin/sh
# Builds ios/build/Thingscan.ipa, signed for the iPhones registered to the signing team, to
# install with Apple Configurator or `xcrun devicectl device install app`. With --install it also
# installs it on the connected iPhone. (Opening the .ipa on the iPhone itself never works.)
#
# A free Personal Team can only sign for iPhones registered with it. Run this with the iPhone
# connected (unlocked, "Trust This Computer" accepted, Developer Mode on) and it registers it.
set -eu
cd "$(dirname "$0")"
out=build
rm -rf "$out"

device=$(xcodebuild -project Thingscan.xcodeproj -scheme Thingscan -showdestinations 2>/dev/null |
  sed -n 's/.*{ platform:iOS, arch:[^,]*, id:\([0-9A-Fa-f-]*\), name:\(.*\) }.*/\1 \2/p' | head -1)
if [ -n "$device" ]; then
  echo "Registering ${device#* } with the signing team…"
  xcodebuild -project Thingscan.xcodeproj -scheme Thingscan -configuration Release \
    -destination "id=${device%% *}" -derivedDataPath "$out/register" \
    -allowProvisioningUpdates -allowProvisioningDeviceRegistration build >/dev/null
else
  echo "No iPhone connected; signing for the iPhones already registered with the team."
fi
xcodebuild -project Thingscan.xcodeproj -scheme Thingscan -configuration Release \
  -destination 'generic/platform=iOS' -archivePath "$out/Thingscan.xcarchive" \
  -allowProvisioningUpdates archive
xcodebuild -exportArchive -archivePath "$out/Thingscan.xcarchive" \
  -exportOptionsPlist ExportOptions.plist -exportPath "$out" -allowProvisioningUpdates
echo "Built $(pwd)/$out/Thingscan.ipa"

if [ "${1:-}" = "--install" ]; then
  if [ -z "$device" ]; then
    echo "No iPhone connected to install on." >&2
    exit 1
  fi
  xcrun devicectl device install app --device "${device%% *}" "$out/Thingscan.ipa"
  echo "Installed on ${device#* }. The first time, trust the developer on the iPhone:"
  echo "Settings > General > VPN & Device Management."
fi
