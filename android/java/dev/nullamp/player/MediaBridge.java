package dev.nullamp.player;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.SystemClock;
import android.util.Log;

/**
 * What Rust talks to. {@code setMetadata} and {@code setPlayback} keep the latest state in static fields and wake the
 * {@link MediaService}, which owns the MediaSession and the notification; {@code nativeCommand} is how the service
 * hands a button press back (implemented by the {@code nullamp-android} library, which also loads this class).
 */
public final class MediaBridge {
    static final int PLAY = 1, PAUSE = 2, TOGGLE = 3, NEXT = 4, PREVIOUS = 5, STOP = 6, SEEK_TO = 7, SEEK_BY = 8;
    static final int STOPPED = 0, PAUSED = 1, PLAYING = 2;
    private static final String TAG = "nullamp";

    /** The track as the lock screen shows it. */
    static final class Meta {
        final String title, artist, album, cover;
        final long durationMs;

        Meta(String title, String artist, String album, long durationMs, String cover) {
            this.title = title;
            this.artist = artist;
            this.album = album;
            this.durationMs = durationMs;
            this.cover = cover;
        }
    }

    /** Play state; {@code at} is when the position was true, so the system can extrapolate it. */
    static final class Play {
        final int state;
        final long positionMs, at;

        Play(int state, long positionMs, long at) {
            this.state = state;
            this.positionMs = positionMs;
            this.at = at;
        }
    }

    static volatile Meta meta = new Meta("Nullamp", null, null, -1, null);
    static volatile Play play = new Play(STOPPED, 0, 0);
    private static boolean askedNotifications;

    static {
        // NativeActivity dlopens the library itself, which does not register it with the class loader; without this
        // the JNI lookup of nativeCommand finds nothing. The library is already mapped, so this only registers it.
        try {
            System.loadLibrary("nullamp_android");
        } catch (UnsatisfiedLinkError e) {
            Log.w(TAG, "cannot load the native library: " + e);
        }
    }

    private static native void nativeCommand(int code, double arg);

    static void send(int code, double arg) {
        try {
            nativeCommand(code, arg);
        } catch (UnsatisfiedLinkError e) {
            Log.w(TAG, "no native command handler: " + e);
        }
    }

    public static void setMetadata(Context context, String title, String artist, String album, long durationMs, String cover) {
        meta = new Meta(title, artist, album, durationMs, cover);
        MediaService.refresh(context.getApplicationContext());
    }

    public static void setPlayback(Context context, int state, long positionMs) {
        play = new Play(state, positionMs, SystemClock.elapsedRealtime());
        MediaService.refresh(context.getApplicationContext());
    }

    /** Android 13 hides the notification, and with it the lock screen player, until the user allows it. Asked once. */
    public static void askForNotifications(Activity activity) {
        if (askedNotifications || Build.VERSION.SDK_INT < 33) {
            return;
        }
        askedNotifications = true;
        if (activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            activity.runOnUiThread(() -> activity.requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 1));
        }
    }

    static Intent serviceIntent(Context context) {
        return new Intent(context, MediaService.class);
    }
}
