#!/bin/sh
# Builds target/release/apk/nullamp.apk for 64-bit ARM phones and emulators. A release build because the debug
# profile decodes too slowly.
#
#   android/build.sh            build
#   android/build.sh install    build, then install on the connected device or emulator
#
# Signing: without CARGO_APK_RELEASE_KEYSTORE the APK is signed with Android Studio's debug key, which is fine for
# trying it out but not for handing to anyone: an update signed with another key cannot replace it. A release is signed
# with the project's own key (see "Releasing" in android/README.md):
#
#   CARGO_APK_RELEASE_KEYSTORE=/path/to/release.jks CARGO_APK_RELEASE_KEYSTORE_PASSWORD=... android/build.sh
#
# Needs: Android SDK with an NDK, `cargo install cargo-apk`, `rustup target add aarch64-linux-android`.
set -e
cd "$(dirname "$0")/.."
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-$(ls -d "$ANDROID_HOME"/ndk/* | tail -1)}"
if [ -z "$CARGO_APK_RELEASE_KEYSTORE" ]; then
    echo "warning: signing with the debug key; set CARGO_APK_RELEASE_KEYSTORE for a release" >&2
    export CARGO_APK_RELEASE_KEYSTORE="$HOME/.android/debug.keystore"
    export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="${CARGO_APK_RELEASE_KEYSTORE_PASSWORD:-android}"
elif [ -z "$CARGO_APK_RELEASE_KEYSTORE_PASSWORD" ]; then
    echo "error: CARGO_APK_RELEASE_KEYSTORE_PASSWORD is not set" >&2
    exit 1
fi
cargo apk build -p nullamp-android --release
if [ "$1" = install ]; then
    "$ANDROID_HOME/platform-tools/adb" install -r target/release/apk/nullamp.apk
fi
