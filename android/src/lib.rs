//! The Android app. cargo-apk builds this crate into a native-activity APK; all the work is in the `nullamp` library.
#![cfg(target_os = "android")]

#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    nullamp::run_android(app);
}
