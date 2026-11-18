package dev.rustel.android;

import android.app.Activity;
import android.media.MediaPlayer;
import android.os.Bundle;
import android.util.Log;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The one-screen Rustel app: edit a score, evaluate it, render it to a WAV
 * file in the app's private storage and play that file.
 *
 * <p>Every native call runs on one background thread, one call at a time:
 * the native handle in {@code crates/android-host/src/jni.rs} borrows the
 * session mutably for each call, so two calls on the same handle must never
 * overlap. {@link #engine} is the single-thread executor that keeps that
 * true; the Evaluate and Render &amp; Play buttons stay disabled while a call
 * is in flight, so calls never queue behind each other either.
 *
 * <p>The engine thread touches no view. Each button reads what it needs from
 * the views on the UI thread, the job runs on the engine thread, and the
 * completion updates the views back on the UI thread.
 */
public final class MainActivity extends Activity {

    private static final String TAG = "Rustel";

    /**
     * The intent extra that makes the activity run one evaluate + render +
     * play cycle on its own after it starts. The headless-emulator check in
     * {@code android/README.md} launches the activity with it so the whole
     * flow leaves evidence in logcat without a touchscreen.
     */
    private static final String EXTRA_AUTORUN = "dev.rustel.AUTORUN";

    /** How much of the installed score Render &amp; Play renders, in seconds. */
    private static final double RENDER_SECONDS = 8.0;

    /** Every native call runs here, one at a time (see the class doc). */
    private final ExecutorService engine = Executors.newSingleThreadExecutor();

    /** The native session handle, 0 until opened. Written on the engine
     * thread, read on the UI thread in onDestroy, hence volatile. */
    private volatile long handle;

    /**
     * The WAV a job wants played once it completes. Set on the engine
     * thread, taken by the completion on the UI thread, hence volatile.
     */
    private volatile File pendingPlayback;

    private EditText scoreView;
    private TextView statusView;
    private TextView diagnosticsView;
    private Button evaluateButton;
    private Button renderButton;
    private Button stopButton;

    /** True while the engine thread runs a call; only touched on the UI
     * thread. */
    private boolean busy;

    private MediaPlayer player;

    /** One unit of work on the engine thread. Returns the diagnostics text
     * to show, and may throw, which the status line reports. */
    private interface EngineJob {
        String run(long handle) throws Exception;
    }

    /**
     * The score the activity opens with. It needs no network: {@code sine}
     * is a synth voice and {@code bd} is the bundled sample.
     */
    private static final String DEFAULT_SCORE =
            "console.log('hello from the score');\n"
            + "$: note(\"c4 e4 g4 c5\").s(\"sine\").gain(0.2)\n"
            + "$: s(\"bd*4\").gain(0.5)\n";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);
        scoreView = findViewById(R.id.score);
        statusView = findViewById(R.id.status);
        diagnosticsView = findViewById(R.id.diagnostics);
        evaluateButton = findViewById(R.id.evaluate);
        renderButton = findViewById(R.id.render);
        stopButton = findViewById(R.id.stop);

        scoreView.setText(DEFAULT_SCORE);

        evaluateButton.setOnClickListener(v -> {
            final String source = scoreView.getText().toString();
            submit("evaluate", h -> {
                RustelHost.evaluate(h, source);
                return RustelHost.diagnosticsJson(h);
            });
        });

        renderButton.setOnClickListener(v -> renderAndPlay(scoreView.getText()
                .toString(), "render & play"));

        // Rendering is one engine call, so a native stop would queue behind
        // it; stopping the playback is what the button can do immediately.
        stopButton.setOnClickListener(v -> stopPlayback());

        if (getIntent().getBooleanExtra(EXTRA_AUTORUN, false)) {
            renderAndPlay(DEFAULT_SCORE, "autorun");
        }
    }

    /** Evaluate {@code source}, render it and leave the WAV for the
     * completion to play. */
    private void renderAndPlay(String source, String what) {
        submit(what, h -> {
            RustelHost.evaluate(h, source);
            Log.i(TAG, what + ": score evaluated");
            File dir = getFilesDir();
            byte[] wav = RustelHost.renderToWav(
                    h, RENDER_SECONDS, dir.getAbsolutePath());
            File file = new File(dir, RustelHost.RENDER_WAV_NAME);
            if (!file.isFile()) {
                writeWav(file, wav);
            }
            Log.i(TAG, what + ": rendered " + wav.length + " wav bytes to "
                    + file.getAbsolutePath());
            pendingPlayback = file;
            return RustelHost.diagnosticsJson(h);
        });
    }

    /** Open the session on the engine thread, where every other call on the
     * handle runs. */
    private long handle() {
        if (handle == 0) {
            handle = RustelHost.newSession();
            Log.i(TAG, "native session " + handle);
        }
        return handle;
    }

    /**
     * Run one job on the engine thread and show what it answered. While a
     * job runs the Evaluate and Render &amp; Play buttons are disabled, so
     * jobs never queue behind each other and the handle sees one call at a
     * time.
     */
    private void submit(String what, EngineJob job) {
        if (busy) {
            return;
        }
        busy = true;
        setButtonsEnabled(false);
        statusView.setText(what + "...");
        engine.execute(() -> {
            String diagnostics;
            boolean ok;
            try {
                diagnostics = job.run(handle());
                ok = true;
            } catch (Throwable error) {
                Log.e(TAG, what + " failed", error);
                diagnostics = error.getMessage();
                ok = false;
            }
            final String text = diagnostics == null
                    ? what + " failed with no message" : diagnostics;
            final boolean done = ok;
            runOnUiThread(() -> {
                busy = false;
                setButtonsEnabled(true);
                statusView.setText(done ? what + " done" : what + " failed");
                diagnosticsView.setText(text);
                File playback = pendingPlayback;
                pendingPlayback = null;
                if (done && playback != null) {
                    play(playback);
                }
            });
        });
    }

    private void setButtonsEnabled(boolean enabled) {
        evaluateButton.setEnabled(enabled);
        renderButton.setEnabled(enabled);
    }

    private void play(File wav) {
        releasePlayer();
        MediaPlayer fresh = new MediaPlayer();
        try {
            fresh.setDataSource(wav.getAbsolutePath());
            fresh.prepare();
            fresh.start();
            player = fresh;
        } catch (IOException error) {
            Log.e(TAG, "playback of " + wav + " failed", error);
            fresh.release();
        }
    }

    /** Stop and forget the current playback, if any. */
    private void releasePlayer() {
        if (player != null) {
            player.stop();
            player.release();
            player = null;
        }
    }

    private void stopPlayback() {
        if (player != null) {
            releasePlayer();
            statusView.setText(R.string.status_idle);
        }
    }

    /**
     * The render writes the WAV itself when it gets an output directory;
     * this is the fallback if the file is missing anyway.
     */
    private static void writeWav(File file, byte[] wav) throws IOException {
        try (FileOutputStream out = new FileOutputStream(file)) {
            out.write(wav);
        }
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        stopPlayback();
        // Queued behind any call still running, so the handle stays
        // single-threaded to the end. shutdown() lets queued work finish.
        long live = handle;
        if (live != 0) {
            engine.execute(() -> RustelHost.dispose(live));
        }
        engine.shutdown();
    }
}
