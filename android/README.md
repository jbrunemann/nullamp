# Nullamp for Android (prototype)

The desktop UI in `app/`, run as a NativeActivity through eframe and wgpu (Vulkan). Only pure Rust goes in: no file
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

There is no file dialog, so **Add > Add folder** adds two folders if they exist: the app's own
`/sdcard/Android/data/dev.nullamp.player/files/Music` (the app creates it at start; `adb push` files into it) and the
shared `/sdcard/Music` (needs the audio permission: `adb shell pm grant dev.nullamp.player android.permission.READ_MEDIA_AUDIO`).

## Known gaps

- Buttons are desktop sized, and hover and right-click interactions do not exist on touch.
- Playback stops with the screen: no foreground service, no audio focus.
- The LCD shows no title at phone width, and its hint text still says "press Ctrl+O".
- Playlist rows cannot be dragged to reorder (a drag scrolls); use the up and down buttons.
- Debug logs: `adb logcat -s nullamp`.
