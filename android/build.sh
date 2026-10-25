#!/bin/sh
# Builds target/release/apk/nullamp.apk for 64-bit ARM phones and emulators, signed with Android Studio's
# debug key (this is a prototype). A release build because the debug profile decodes too slowly.
#
#   android/build.sh            build
#   android/build.sh install    build, then install on the connected device or emulator
#
# Needs: Android SDK with an NDK, `cargo install cargo-apk`, `rustup target add aarch64-linux-android`.
set -e
cd "$(dirname "$0")/.."
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-$(ls -d "$ANDROID_HOME"/ndk/* | tail -1)}"
export CARGO_APK_RELEASE_KEYSTORE="${CARGO_APK_RELEASE_KEYSTORE:-$HOME/.android/debug.keystore}"
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="${CARGO_APK_RELEASE_KEYSTORE_PASSWORD:-android}"
cargo apk build -p nullamp-android --release
if [ "$1" = install ]; then
    "$ANDROID_HOME/platform-tools/adb" install -r target/release/apk/nullamp.apk
fi
