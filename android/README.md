# Nullamp for Android (prototype)

The desktop UI in `app/`, run as a NativeActivity through eframe and wgpu (Vulkan). Only pure Rust goes in: no system file
picker, no media session, no SQLite, no `curl`. That leaves out the rekordbox desktop library and the Mixxx library
(C, SQLite), and the lock screen controls (need Java).
The online features (cover lookup, update check, Shazam, 1001Tracklists) ask over HTTPS through `nullamp-http`, which uses
`ureq` and rustls here (`ring` is C, built by the NDK) because Android has no `curl`. They need the INTERNET permission.

## Build and run

```sh
rustup target add aarch64-linux-android
cargo install cargo-apk
sdkmanager --install "ndk;27.3.13750724"      # any recent NDK
android/build.sh install                      # builds target/release/apk/nullamp.apk, installs it with adb
adb shell am start -n dev.nullamp.player/android.app.NativeActivity
```

## Releasing

`android/build.sh` signs with Android Studio's debug key unless `CARGO_APK_RELEASE_KEYSTORE` names a keystore, and then
it needs `CARGO_APK_RELEASE_KEYSTORE_PASSWORD` too. The release key is made once and never changes, because Android
refuses to update an app with an APK signed by another key:

```sh
keytool -genkeypair -v -keystore nullamp-release.jks -alias nullamp -keyalg RSA -keysize 4096 -validity 36500
```

Keep the file and its password somewhere safe and out of the repository (losing it means every user must uninstall).
The release workflow (`.github/workflows/release.yml`, on a `v*` tag) builds `Nullamp-<version>-android-arm64.apk` with a
`.sha256` file and attaches them to the release. It reads the key from two repository secrets:
`ANDROID_KEYSTORE_BASE64` (`base64 -i nullamp-release.jks`) and `ANDROID_KEYSTORE_PASSWORD`. `apksigner` takes the only key in the keystore, and
reads its password from the store's, so give both the same password (`keytool` offers that by default).

`cargo-apk` takes the version name from the workspace version and makes the version code from it
(`1 << 24 | major << 16 | minor << 8 | patch`, so 2.0.0 is 16973824), which grows with every release; the TOML must not set
either.

## Getting music in

Android has no native file dialog that Rust can reach (the system picker hands its answer back through Java), so
**Add > Add files**, **Add folder**, the playlist and setlist imports and exports and the artwork picker all open
`app/src/ui/file_browser.rs`, a browser drawn by egui with shortcuts for Music, Download, internal storage, the app's
own folder and memory cards. It lists any folder, which needs the **All files access** permission
(`MANAGE_EXTERNAL_STORAGE`); the browser and Settings > General show a button that opens the system page for it.
The same permission lets tag saves rewrite files other apps or a computer put there. For a quick test without the
page: `adb shell appops set dev.nullamp.player MANAGE_EXTERNAL_STORAGE allow`. Play Store does not give this
permission to most apps, so a store build would need the system picker and Java after all.

## Known gaps

- Buttons are desktop sized, and hover and right-click interactions do not exist on touch.
- Playback stops with the screen: no foreground service, no audio focus.
- The LCD shows no title at phone width, and its hint text still says "press Ctrl+O".
- Playlist rows cannot be dragged to reorder (a drag scrolls); use the up and down buttons.
- Debug logs: `adb logcat -s nullamp`.
