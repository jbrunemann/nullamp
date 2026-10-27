//! The Android app. cargo-apk builds this crate into a native-activity APK; all the work is in the `nullamp` library.
#![cfg(target_os = "android")]

#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    nullamp::run_android(app);
}

/// `MediaBridge.nativeCommand(int, double)` of `android/java/`: a press on the lock screen, the notification or a
/// headset. The JNI name has to be found in this library, which is why it is not in `nullamp-sysmedia`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_nullamp_player_MediaBridge_nativeCommand(_env: *mut std::ffi::c_void, _class: *mut std::ffi::c_void, code: i32, arg: f64) {
    nullamp_sysmedia::on_native_command(code, arg);
}
