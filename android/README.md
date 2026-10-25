# Nullamp for Android (prototype)

The desktop UI in `app/`, run as a NativeActivity through eframe and wgpu (Vulkan). Only pure Rust goes in: no system file
picker, no media session, no SQLite, no `curl`. That leaves out the rekordbox desktop library and the Mixxx library
(C, SQLite), online lookups (`curl`), and the lock screen controls (need Java).

## Build and run

```sh
rustup target add aarch64-linux-android
cargo install cargo-apk
sdkmanager --install "ndk;27.3.13750724"      # any recent NDK
android/build.sh install                      # builds target/release/apk/nullamp.apk, installs it with adb
adb shell am start -n dev.nullamp.player/android.app.NativeActivity
```

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
