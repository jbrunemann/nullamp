//! The Android backend: `MediaBridge` and `MediaService` in `android/java/` do the work (a MediaSession, a
//! notification, audio focus, a foreground service), this side mirrors the player's state into them and turns their
//! callbacks into [`SystemEvent`]s.
//!
//! Calls go to static methods of `dev.nullamp.player.MediaBridge` with the application as the context. Presses come
//! back through `MediaBridge.nativeCommand`, a native method the `nullamp-android` library exports as
//! `Java_dev_nullamp_player_MediaBridge_nativeCommand` and forwards to [`on_native_command`], on the main thread of
//! the VM.

use std::mem::ManuallyDrop;
use std::sync::Mutex;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::sys::jobject;
use jni::{Env, JavaVM, jni_sig, jni_str};

use crate::{NowPlaying, PlaybackState, SystemEvent, command_from_code};

type Handler = Box<dyn Fn(SystemEvent) + Send>;

static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);

/// A press on the lock screen, the notification or a headset, from the Java side.
pub fn on_native_command(code: i32, arg: f64) {
    log::debug!("media command {code} ({arg})");
    let Some(command) = command_from_code(code, arg) else {
        log::debug!("unknown media command {code}");
        return;
    };
    if let Ok(handler) = HANDLER.lock()
        && let Some(handler) = handler.as_ref()
    {
        handler(SystemEvent::Command(command));
    }
}

/// Asks for the notification permission of Android 13, without which the lock screen player stays hidden. Needs the
/// activity itself (`ndk-context` only has the application), so the caller passes it.
pub fn ask_for_notifications(activity: *mut std::ffi::c_void) {
    call_with(activity, |env, activity| {
        let bridge = bridge(env, activity)?;
        env.call_static_method(&bridge, jni_str!("askForNotifications"), jni_sig!("(Landroid/app/Activity;)V"), &[activity.into()])?;
        Ok(())
    });
}

pub struct SystemMedia {
    last_metadata: Option<NowPlaying>,
    last_state: Option<PlaybackState>,
}

impl SystemMedia {
    /// `handler` may be called from any thread. The service itself starts with the first playback.
    pub fn new(handler: impl Fn(SystemEvent) + Send + 'static, _hwnd: Option<*mut std::ffi::c_void>) -> Self {
        if let Ok(mut slot) = HANDLER.lock() {
            *slot = Some(Box::new(handler));
        }
        SystemMedia { last_metadata: None, last_state: None }
    }

    pub fn is_available(&self) -> bool {
        true
    }

    pub fn set_metadata(&mut self, now: &NowPlaying) {
        if self.last_metadata.as_ref() == Some(now) {
            return;
        }
        let duration_ms = now.duration.filter(|d| d.is_finite() && *d > 0.0).map_or(-1, |d| (d * 1000.0) as i64);
        let cover = now.cover.as_ref().map(|p| p.to_string_lossy().into_owned());
        call(|env, context| {
            let title = env.new_string(&now.title)?;
            let artist = optional(env, now.artist.as_deref())?;
            let album = optional(env, now.album.as_deref())?;
            let cover = optional(env, cover.as_deref())?;
            let bridge = bridge(env, context)?;
            env.call_static_method(
                &bridge,
                jni_str!("setMetadata"),
                jni_sig!("(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JLjava/lang/String;)V"),
                &[context.into(), (&title).into(), (&artist).into(), (&album).into(), JValue::Long(duration_ms), (&cover).into()],
            )?;
            Ok(())
        });
        self.last_metadata = Some(now.clone());
        // The service resets the position with new metadata on some versions; send the state again.
        if let Some(state) = self.last_state.take() {
            self.set_playback(state);
        }
    }

    pub fn clear_metadata(&mut self) {
        self.set_metadata(&NowPlaying { title: "Nullamp".into(), ..Default::default() });
    }

    /// Call on state changes and seeks, not continuously: the system extrapolates the position while playing.
    pub fn set_playback(&mut self, state: PlaybackState) {
        if self.last_state == Some(state) {
            return;
        }
        let (code, position) = match state {
            PlaybackState::Stopped => (0, 0.0),
            PlaybackState::Paused { position } => (1, position),
            PlaybackState::Playing { position } => (2, position),
        };
        let position_ms = (position.max(0.0) * 1000.0) as i64;
        call(|env, context| {
            let bridge = bridge(env, context)?;
            env.call_static_method(
                &bridge,
                jni_str!("setPlayback"),
                jni_sig!("(Landroid/content/Context;IJ)V"),
                &[context.into(), JValue::Int(code), JValue::Long(position_ms)],
            )?;
            Ok(())
        });
        self.last_state = Some(state);
    }
}

/// `FindClass` on a native thread only sees system classes, so the app's class comes from the activity's loader.
fn bridge<'local>(env: &mut Env<'local>, context: &JObject) -> jni::errors::Result<JClass<'local>> {
    let loader = env.call_method(context, jni_str!("getClassLoader"), jni_sig!("()Ljava/lang/ClassLoader;"), &[])?.l()?;
    let name = env.new_string("dev.nullamp.player.MediaBridge")?;
    let class = env.call_method(&loader, jni_str!("loadClass"), jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"), &[(&name).into()])?.l()?;
    env.cast_local::<JClass>(class)
}

fn optional<'local>(env: &mut Env<'local>, text: Option<&str>) -> jni::errors::Result<JString<'local>> {
    match text {
        Some(t) => env.new_string(t),
        None => Ok(JString::null()),
    }
}

/// Runs `f` with the application as the context. Failures are logged and the pending Java exception cleared, since
/// the player works without the system integration.
fn call(f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<()>) {
    call_with(ndk_context::android_context().context().cast(), f)
}

fn call_with(context: *mut std::ffi::c_void, f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<()>) {
    let vm = ndk_context::android_context().vm();
    if vm.is_null() || context.is_null() {
        return;
    }
    // SAFETY: the pointer is the VM the activity runs in, valid for the life of the process.
    let vm = unsafe { JavaVM::from_raw(vm.cast()) };
    let result = vm.attach_current_thread(|env| {
        // SAFETY: the context is a global reference that android-activity owns and that is never deleted here.
        let context = ManuallyDrop::new(unsafe { JObject::from_raw(env, context as jobject) });
        let result = f(env, &context);
        if result.is_err() {
            env.exception_clear();
        }
        result
    });
    if let Err(e) = result {
        log::warn!("media session: {e}");
    }
}
