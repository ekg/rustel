package dev.rustel.android;

import android.util.Log;

/**
 * The Java half of the native library {@code rustel_android_host}, which
 * {@code build-apk.sh} builds with cargo-ndk and packs into the APK's
 * {@code lib/<abi>/} directories. The method names, signatures and the
 * class name are fixed by the JNI exports in
 * {@code crates/android-host/src/jni.rs}; the library is found by
 * {@link System#loadLibrary} because its file name is
 * {@code librustel_android_host.so}.
 */
public final class RustelHost {

    private static final String TAG = "Rustel";

    /** The file {@link #renderToWav} writes inside its {@code outDir}
     * argument: {@code <dir>/rustel-render.wav}. */
    public static final String RENDER_WAV_NAME = "rustel-render.wav";

    static {
        System.loadLibrary("rustel_android_host");
        Log.i(TAG, "rustel_android_host loaded");
    }

    private RustelHost() {
    }

    /**
     * Opens a session; the handle is its native address. Throws on failure
     * and returns 0.
     */
    public static native long newSession();

    /** Evaluates a score source. Throws on a failed evaluation. */
    public static native void evaluate(long handle, String source);

    /**
     * Renders {@code durationSecs} of the installed score to a WAV byte
     * array; a non-null {@code outDir} also receives the bytes as
     * {@code outDir/rustel-render.wav}.
     */
    public static native byte[] renderToWav(long handle, double durationSecs,
                                            String outDir);

    /**
     * Stops the transport; the next render starts it again. Only call it
     * when no other call on the handle is running.
     */
    public static native void stop(long handle);

    /** Drains pending diagnostics as a JSON array string. */
    public static native String diagnosticsJson(long handle);

    /**
     * Rewrites a score through one OpenAI-compatible chat call:
     * {@code endpoint} is the base URL ahead of {@code /chat/completions},
     * and the reply is the complete new score text. Throws with the
     * failure's message when the call fails.
     */
    public static native String llmRewrite(long handle, String endpoint,
                                          String model, String key,
                                          String score, String prompt);

    /**
     * Closes the session and frees the handle. Only call it when no other
     * call on the handle is running.
     */
    public static native void dispose(long handle);
}
