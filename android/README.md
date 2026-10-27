# Nullamp for Android (prototype)

The desktop UI in `app/`, run as a NativeActivity through eframe and wgpu (Vulkan). Apart from the media service below
only pure Rust goes in: no system file picker, no SQLite, no `curl`. That leaves out the rekordbox desktop library and the
Mixxx library (C, SQLite).
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

## Media session and background playback

The lock screen and notification player, headset, Bluetooth and car buttons, audio focus and playback with the screen
off need a foreground service and a `MediaSession`, which are Java classes. They are two small files in `android/java/`
(no Gradle, no Media3, only the platform API):

- `MediaService`: a foreground service of type `mediaPlayback` (API 34) that owns the `MediaSession`, the media style
  notification (previous, play/pause, next), audio focus (a lasting loss pauses, a short one pauses and resumes), the
  pause on unplugged headphones and a partial wake lock while playing. Paused, the notification can be swiped away;
  stopped, the service ends.
- `MediaBridge`: what Rust calls (`setMetadata`, `setPlayback`, `askForNotifications`) and the native method
  (`nativeCommand`) the service calls back for every press.

`crates/nullamp-sysmedia/src/android.rs` is the Rust side (JNI through the application context, the class found
through its class loader). `Java_dev_nullamp_player_MediaBridge_nativeCommand` is exported by `android/src/lib.rs`;
`MediaBridge` calls `System.loadLibrary`, because NativeActivity loads the library without registering it with the
class loader. `cargo-apk` can add neither a service nor a `classes.dex`, so `android/package.sh` (run by `build.sh`)
compiles the Java with `javac` and `d8`, adds the service to the manifest cargo-apk wrote, and packs, aligns and signs
the APK again.

eframe drops its window while the activity is stopped and runs no frames, so `App::logic` (next track, presses from
the service) would stop. `ui::background::Shared` holds the app behind a mutex and a thread runs `process_events` every
200 ms while no frame has run for 600 ms.

Android 13 and later need the user to allow notifications for the player to show; the prompt comes with the first
playback. Without it the session still works (headset buttons, background playback), only the notification is hidden.

Test without a headset: `adb shell cmd media_session dispatch play|pause|next|previous`.

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

## Store build

`android/build.sh store` builds with `--no-default-features`: no Identify with Shazam, Search 1001Tracklists, Request on
Telegram and no Settings > Updates, for an app store that does its own updates (F-Droid). The code is still in the
library, only the menus and tabs that reach it are left out; the cover lookup of a mix (Deezer, iTunes) stays.

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
- Audio focus and the notification swipe-away are untested on a device (the emulator's simulated call does not take
  focus).
- The LCD shows no title at phone width, and its hint text still says "press Ctrl+O".
- Playlist rows cannot be dragged to reorder (a drag scrolls); use the up and down buttons.
- Debug logs: `adb logcat -s nullamp`.
