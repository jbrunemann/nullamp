package dev.nullamp.player;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.drawable.Icon;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.os.SystemClock;
import android.util.Log;

import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Keeps playback alive and visible while the app is in the background: a foreground service of type mediaPlayback
 * (its notification is what stops Android from freezing the process), a MediaSession (lock screen, headset, Bluetooth
 * and car buttons, the system media controls), audio focus (a call or another player pauses us) and a wake lock for
 * the decoder threads. The audio itself plays in Rust; this class only mirrors its state and forwards presses.
 */
public final class MediaService extends Service {
    private static final String TAG = "nullamp";
    private static final String CHANNEL = "playback";
    private static final int NOTIFICATION_ID = 1;
    private static final String ACTION_PREFIX = "dev.nullamp.player.";
    private static final String DISMISS = ACTION_PREFIX + "DISMISS";
    private static final double SKIP_SECONDS = 10;
    private static final int COVER_PIXELS = 512;

    private static MediaService instance;

    private final Handler main = new Handler(Looper.getMainLooper());
    private final ExecutorService covers = Executors.newSingleThreadExecutor();
    private MediaSession session;
    private NotificationManager notifications;
    private AudioManager audio;
    private AudioFocusRequest focus;
    private PowerManager.WakeLock wake;
    private boolean foreground, resumeOnFocus;
    private String coverPath;
    private Bitmap cover;
    private BroadcastReceiver noisy;

    /** Called from any thread when the bridge's state changed. */
    static void refresh(Context context) {
        MediaService running = instance;
        if (running != null) {
            running.main.post(running::apply);
        } else if (MediaBridge.play.state == MediaBridge.PLAYING) {
            try {
                context.startForegroundService(MediaBridge.serviceIntent(context));
            } catch (RuntimeException e) {
                // Android 12 refuses a foreground service started from the background; the app is then not playing
                // by its own doing, and the next press on the lock screen or headset starts it.
                Log.w(TAG, "cannot start the media service: " + e);
            }
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        instance = this;
        notifications = getSystemService(NotificationManager.class);
        audio = getSystemService(AudioManager.class);
        notifications.createNotificationChannel(new NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW));
        PowerManager power = getSystemService(PowerManager.class);
        wake = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nullamp:playback");
        wake.setReferenceCounted(false);

        session = new MediaSession(this, "Nullamp");
        session.setCallback(new MediaSession.Callback() {
            @Override public void onPlay() { MediaBridge.send(MediaBridge.PLAY, 0); }
            @Override public void onPause() { MediaBridge.send(MediaBridge.PAUSE, 0); }
            @Override public void onStop() { MediaBridge.send(MediaBridge.STOP, 0); }
            @Override public void onSkipToNext() { MediaBridge.send(MediaBridge.NEXT, 0); }
            @Override public void onSkipToPrevious() { MediaBridge.send(MediaBridge.PREVIOUS, 0); }
            @Override public void onSeekTo(long positionMs) { MediaBridge.send(MediaBridge.SEEK_TO, positionMs / 1000.0); }
            @Override public void onFastForward() { MediaBridge.send(MediaBridge.SEEK_BY, SKIP_SECONDS); }
            @Override public void onRewind() { MediaBridge.send(MediaBridge.SEEK_BY, -SKIP_SECONDS); }
        }, main);
        session.setSessionActivity(openApp());

        noisy = new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                if (MediaBridge.play.state == MediaBridge.PLAYING) {
                    MediaBridge.send(MediaBridge.PAUSE, 0);
                }
            }
        };
        registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        if (action != null && action.startsWith(ACTION_PREFIX)) {
            switch (action) {
                case ACTION_PREFIX + "PLAY": MediaBridge.send(MediaBridge.PLAY, 0); break;
                case ACTION_PREFIX + "PAUSE": MediaBridge.send(MediaBridge.PAUSE, 0); break;
                case ACTION_PREFIX + "NEXT": MediaBridge.send(MediaBridge.NEXT, 0); break;
                case ACTION_PREFIX + "PREVIOUS": MediaBridge.send(MediaBridge.PREVIOUS, 0); break;
                case DISMISS: dismissed(); break;
                default: break;
            }
            return START_NOT_STICKY;
        }
        // Started by startForegroundService: the notification must go up within seconds, whatever the state is by now.
        if (!foreground) {
            enterForeground(notification());
        }
        apply();
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public void onDestroy() {
        instance = null;
        unregisterReceiver(noisy);
        releaseFocus();
        releaseWake();
        session.setActive(false);
        session.release();
        covers.shutdownNow();
        super.onDestroy();
    }

    /** The user swiped the notification away (only possible while paused): stop showing, keep the player as it is. */
    private void dismissed() {
        if (MediaBridge.play.state != MediaBridge.PLAYING) {
            session.setActive(false);
            stopSelf();
        }
    }

    private void apply() {
        MediaBridge.Meta meta = MediaBridge.meta;
        MediaBridge.Play play = MediaBridge.play;
        if (play.state == MediaBridge.STOPPED) {
            releaseFocus();
            releaseWake();
            session.setActive(false);
            stopForeground(STOP_FOREGROUND_REMOVE);
            foreground = false;
            stopSelf();
            return;
        }
        loadCover(meta.cover);
        session.setMetadata(metadata(meta));
        session.setPlaybackState(playbackState(play));
        session.setActive(true);
        Notification n = notification();
        if (play.state == MediaBridge.PLAYING) {
            resumeOnFocus = false;
            holdWake();
            if (!foreground) {
                enterForeground(n);
            } else {
                notifications.notify(NOTIFICATION_ID, n);
            }
            if (!holdFocus()) {
                MediaBridge.send(MediaBridge.PAUSE, 0);
            }
        } else {
            releaseWake();
            if (foreground) {
                // Keeps the notification, now one the user can swipe away.
                stopForeground(STOP_FOREGROUND_DETACH);
                foreground = false;
            }
            notifications.notify(NOTIFICATION_ID, n);
        }
    }

    private void enterForeground(Notification n) {
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
            } else {
                startForeground(NOTIFICATION_ID, n);
            }
            foreground = true;
        } catch (RuntimeException e) {
            Log.w(TAG, "cannot go foreground: " + e);
        }
    }

    private MediaMetadata metadata(MediaBridge.Meta meta) {
        MediaMetadata.Builder b = new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, meta.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, meta.artist)
                .putString(MediaMetadata.METADATA_KEY_ALBUM, meta.album);
        if (meta.durationMs > 0) {
            b.putLong(MediaMetadata.METADATA_KEY_DURATION, meta.durationMs);
        }
        if (cover != null) {
            b.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, cover);
        }
        return b.build();
    }

    private PlaybackState playbackState(MediaBridge.Play play) {
        long actions = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE
                | PlaybackState.ACTION_SKIP_TO_NEXT | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_STOP
                | PlaybackState.ACTION_SEEK_TO | PlaybackState.ACTION_FAST_FORWARD | PlaybackState.ACTION_REWIND;
        boolean playing = play.state == MediaBridge.PLAYING;
        return new PlaybackState.Builder()
                .setActions(actions)
                .setState(playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED, play.positionMs, playing ? 1f : 0f, play.at)
                .build();
    }

    private Notification notification() {
        MediaBridge.Meta meta = MediaBridge.meta;
        boolean playing = MediaBridge.play.state == MediaBridge.PLAYING;
        Notification.Builder b = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(icon("ic_stat_nullamp"))
                .setContentTitle(meta.title)
                .setContentText(meta.artist)
                .setSubText(meta.album)
                .setLargeIcon(cover)
                .setContentIntent(openApp())
                .setDeleteIntent(action(DISMISS))
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setCategory(Notification.CATEGORY_TRANSPORT)
                .setShowWhen(false)
                .setOnlyAlertOnce(true)
                .setOngoing(playing)
                .addAction(button("ic_previous", "Previous", "PREVIOUS"))
                .addAction(playing ? button("ic_pause", "Pause", "PAUSE") : button("ic_play", "Play", "PLAY"))
                .addAction(button("ic_next", "Next", "NEXT"))
                .setStyle(new Notification.MediaStyle().setMediaSession(session.getSessionToken()).setShowActionsInCompactView(0, 1, 2));
        return b.build();
    }

    private Notification.Action button(String drawable, String title, String name) {
        return new Notification.Action.Builder(icon(drawable), title, action(ACTION_PREFIX + name)).build();
    }

    private Icon icon(String drawable) {
        return Icon.createWithResource(this, getResources().getIdentifier(drawable, "drawable", getPackageName()));
    }

    private PendingIntent action(String name) {
        Intent intent = MediaBridge.serviceIntent(this).setAction(name);
        return PendingIntent.getService(this, 0, intent, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    private PendingIntent openApp() {
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        return PendingIntent.getActivity(this, 0, launch, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    /** Decodes the cover off the main thread, at a size a notification and a lock screen need, then shows it. */
    private void loadCover(String path) {
        if (path == null ? coverPath == null : path.equals(coverPath)) {
            return;
        }
        coverPath = path;
        cover = null;
        if (path == null) {
            return;
        }
        covers.execute(() -> {
            Bitmap bitmap = decode(path);
            main.post(() -> {
                if (path.equals(coverPath) && bitmap != null) {
                    cover = bitmap;
                    apply();
                }
            });
        });
    }

    private static Bitmap decode(String path) {
        BitmapFactory.Options bounds = new BitmapFactory.Options();
        bounds.inJustDecodeBounds = true;
        BitmapFactory.decodeFile(path, bounds);
        int sample = 1;
        while (Math.max(bounds.outWidth, bounds.outHeight) / (sample * 2) >= COVER_PIXELS) {
            sample *= 2;
        }
        BitmapFactory.Options options = new BitmapFactory.Options();
        options.inSampleSize = sample;
        return BitmapFactory.decodeFile(path, options);
    }

    // ----- Audio focus and wake lock -----

    /** False when the system refuses focus, as it does during a call. */
    private boolean holdFocus() {
        if (focus != null) {
            return true;
        }
        focus = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build())
                .setOnAudioFocusChangeListener(this::focusChanged, main)
                .build();
        if (audio.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_FAILED) {
            focus = null;
            return false;
        }
        return true;
    }

    private void releaseFocus() {
        if (focus != null) {
            audio.abandonAudioFocusRequest(focus);
            focus = null;
        }
    }

    /** A short interruption (a navigation prompt, a call ringing) pauses and resumes; a lasting one only pauses. Ducking the system does itself. */
    private void focusChanged(int change) {
        boolean playing = MediaBridge.play.state == MediaBridge.PLAYING;
        switch (change) {
            case AudioManager.AUDIOFOCUS_LOSS:
                resumeOnFocus = false;
                if (playing) {
                    MediaBridge.send(MediaBridge.PAUSE, 0);
                }
                releaseFocus();
                break;
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
                if (playing) {
                    resumeOnFocus = true;
                    MediaBridge.send(MediaBridge.PAUSE, 0);
                }
                break;
            case AudioManager.AUDIOFOCUS_GAIN:
                if (resumeOnFocus) {
                    resumeOnFocus = false;
                    MediaBridge.send(MediaBridge.PLAY, 0);
                }
                break;
            default:
                break;
        }
    }

    private void holdWake() {
        if (!wake.isHeld()) {
            wake.acquire();
        }
    }

    private void releaseWake() {
        if (wake.isHeld()) {
            wake.release();
        }
    }
}
