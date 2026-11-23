# Rustel Android app

A minimal single-activity Android app that embeds the Rustel engine through
the `rustel-android-host` native library (`crates/android-host`): edit a
score, evaluate it, render it to a WAV file in the app's private storage,
play it back, and ask an OpenAI-compatible model to rewrite the score.
There is no Gradle involved; a shell script drives the SDK tools directly.

The app is deliberately small: one `EditText` with a default score, an
**Evaluate** button (score → engine diagnostics in a `TextView`), a
**Render & Play** button (render 8 s of the installed score to
`files/rustel-render.wav` and play it with `MediaPlayer`), a **Stop**
button that stops the playback, and the **Ask** flow described below. All
native calls are funnelled through one background thread, because the JNI
handle borrows the session mutably for each call and must never see two
calls overlap.

## What it runs on

- **minSdk 21**: the `rustel-android-host` cdylib links against the NDK's
  API-21 libraries (see the Android section of `docs/building.md`), so 21 is
  the floor the built library actually requires. The manifest also requests
  `INTERNET`, which a score that fetches samples over HTTPS needs; offline
  renders do not use it.
- **ABIs**: `arm64-v8a` for devices, plus `x86_64` for the emulator, which
  `build-apk.sh` includes whenever the `x86_64-linux-android` rust target is
  installed.
- **16 KB page alignment**: NDK r30's lld links the shared library with
  `-Wl,-z,max-page-size=16384` by default, which is what 16 KB-page devices
  (Android 15 on recent arm64 hardware) require. If you build with an older
  NDK, add that flag to the link step explicitly.

## Building the APK

Requirements on the machine:

- Android SDK with `build-tools/34.0.0` and `platforms/android-34`
  (`ANDROID_SDK_ROOT`, default `/usr/lib/android-sdk`),
- Android NDK r30 (`ANDROID_NDK_HOME`,
  default `/home/erik/android-sdk/ndk/30.0.16248370`),
- `cargo-ndk`, with the rust targets `aarch64-linux-android` (required) and
  `x86_64-linux-android` (optional, for the emulator),
- a JDK (javac, keytool) and `zip`.

Then, from anywhere:

```sh
android/build-apk.sh            # release build (default)
android/build-apk.sh debug      # debug build, faster to compile
```

The script:

1. runs `cargo ndk` for every ABI, through the dash-spelled
   `BINDGEN_EXTRA_CLANG_ARGS_<triple>` wrapper the bindgen build needs (see
   the Android cross-build section of `docs/building.md`),
2. compiles and links the resources and the manifest with `aapt2`,
3. compiles the Java sources with `javac --release 8` against
   `platforms/android-34/android.jar` and dexes them with `d8 --min-api 21`,
4. packs `classes.dex` and `lib/<abi>/librustel_android_host.so` into the
   APK, `zipalign`s it and signs it with a generated debug keystore
   (`android/build/debug.keystore`, created once and reused).

The signed APK lands at `android/build/rustel.apk`. Everything under
`android/build/` is scratch output; only the keystore survives between runs
so that repeated builds sign with the same key.

## Running it on a device

1. Enable developer options and USB debugging on the device.
2. `adb install -r android/build/rustel.apk`
3. Open the **Rustel** launcher entry, edit the score, press **Evaluate**
   (diagnostics appear under the buttons), then **Render & Play**.
   The render writes `files/rustel-render.wav` in the app's private storage
   and plays it back.

To inspect a render from the host: `adb shell run-as dev.rustel.android
cat files/rustel-render.wav > render.wav` (the app is debuggable-signed, so
`run-as` works on a non-release device).

## Asking a model to rewrite the score

Above the buttons sit three small fields - **endpoint** (the base URL
ahead of `/chat/completions`, for example `https://api.openai.com/v1`),
**model**, and **API key** - and a **prompt** field with an **Ask**
button next to the score editor. Any OpenAI-compatible chat endpoint
works: the hosted APIs, but also a local `ollama serve`, a `llama.cpp`
server, or the mock server below.

**Ask** sends the current score and the prompt to the model through the
`rustel-pod-llm` crate (the same client the studio's llm pod uses, with
the Strudel system prompt), and on success puts the complete rewritten
score back into the editor and evaluates it through the same path the
**Evaluate** button uses, so the diagnostics under the buttons show the
rewritten score's engine output. On failure the diagnostics view shows
the error text, which names the kind of failure (HTTP status, transport,
malformed JSON, empty reply); the score in the editor is untouched. The
**Ask** button is disabled while any native call is in flight, like the
other buttons.

What the model is told about the language is the reference of the build the
app carries: every name the engine installs, with its signature and a
one-line description, generated from the same tables the studio's reference
panel reads. The **full reference** checkbox under the prompt chooses between
that one-line sheet - unticked, the default, and what a fresh install sends -
and the same sheet with every parameter's type and description. The choice
is saved with the endpoint, model and key.

The reference is the same bytes on every call, and only the score and the
prompt change between them, which is what a provider's prefix cache reuses:
the cached part of a request is billed at a fraction of the input price.
Ticking or unticking **full reference**, or installing a new build, changes
the prefix, and the first call after that pays for the whole message again.

Caveats:

- **The endpoint, model and key are stored in plaintext** in the app's
own `SharedPreferences` (`llm` file), because the app has no encrypted
storage of its own. Anything with access to the app's private data (a
rooted device, an `adb backup` on older devices) can read the key. Use a
scrap key for experiments, or keep real keys off this debug-signed app.
- **Cleartext HTTP is allowed** (`android:usesCleartextTraffic` in the
manifest), because the interesting local endpoints - the emulator's host
loopback, a LAN `ollama` or `llama.cpp` server - are plain `http://`.
Prefer `https://` for real services; with cleartext allowed, a
man-in-the-middle can read the key and the score traffic.
- On the emulator, the host machine's loopback is reachable as
`http://10.0.2.2:<port>`; `127.0.0.1` inside the emulator is the emulator
itself.

## Running the headless emulator check

With KVM available and the `system-images;android-34;default;x86_64` image
installed, the whole flow can run without a screen:

```sh
AVD_NAME=rustel-test
avdmanager create avd -n $AVD_NAME \
    -k "system-images;android-34;default;x86_64" -d pixel
emulator -avd $AVD_NAME -no-window -no-audio -gpu swiftshader_indirect \
    -no-snapshot -no-boot-anim &
adb wait-for-device
adb shell 'while [ "$(getprop sys.boot_completed)" != "1" ]; do sleep 2; done'
adb install -r android/build/rustel.apk
# The extra makes the activity evaluate, render and play once on its own.
adb shell am start -n dev.rustel.android/.MainActivity \
    --ez dev.rustel.AUTORUN true
adb logcat -d -s Rustel
```

The `Rustel` log tag then shows the whole sequence: the native library
loading, the session opening, the score evaluating, the diagnostics JSON, and
the rendered WAV byte count.

### The headless LLM check

The Ask flow has its own headless run, on the same pattern: a canned
OpenAI-compatible server runs on the host, and an intent extra makes the
activity fill its endpoint, model and prompt fields and press Ask on its
own. No Termux or touchscreen is involved; the app itself makes the call.

```sh
# On the host: one canned rewrite, served on 127.0.0.1:8081.
android/llm-mock-server.py 8081 &

# In the emulator: 10.0.2.2 is the host's loopback.
adb shell am start -n dev.rustel.android/.MainActivity \
    --ez dev.rustel.LLM_AUTORUN true \
    --es dev.rustel.LLM_ENDPOINT http://10.0.2.2:8081 \
    --es dev.rustel.LLM_MODEL mock-model \
    --es dev.rustel.LLM_PROMPT "arpeggiate the melody"
sleep 5
adb logcat -d -s Rustel
adb shell uiautomator dump /sdcard/llm-proof.xml
adb pull /sdcard/llm-proof.xml
```

The `Rustel` log tag shows `ask: asking mock-model at http://10.0.2.2:8081`
and `ask: adopted the rewritten score (… chars)`, the mock's own log shows
the `POST /chat/completions` request, and the dumped UI XML shows the score
`EditText` holding the mock's rewritten score (its `console.log('rewritten
by the mock llm')` marker line) and the diagnostics `TextView` carrying the
engine's evaluation of it - the same evidence a tap on **Ask** produces.

## Known limits

- The **Stop** button stops the playback, not a render in progress:
  rendering is one native call on the engine thread, and a native `stop`
  would have to queue behind it. The host's `stop` export stays available to
  future live-audio wiring (`docs/building.md` records the AAudio story).
- The app renders with the bundled `bd` sample and synth voices only; the
  engine's network sample bank is not enabled behind the app's back
  (see `crates/android-host/src/lib.rs`).
- **Ask is Mode A only**: one rewrite call per prompt, no agent loop. The
  agent pod (Mode B) remains the studio's Termux path
  (`docs/studio.md`); the app has no agent mode.
