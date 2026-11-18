# Rustel Android app

A minimal single-activity Android app that embeds the Rustel engine through
the `rustel-android-host` native library (`crates/android-host`): edit a
score, evaluate it, render it to a WAV file in the app's private storage and
play it back. There is no Gradle involved; a shell script drives the SDK
tools directly.

The app is deliberately small: one `EditText` with a default score, an
**Evaluate** button (score → engine diagnostics in a `TextView`), a
**Render & Play** button (render 8 s of the installed score to
`files/rustel-render.wav` and play it with `MediaPlayer`), and a **Stop**
button that stops the playback. All native calls are funnelled through one
background thread, because the JNI handle borrows the session mutably for
each call and must never see two calls overlap.

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
   `docs/building.md` and the probe report referenced there),
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

## Known limits

- The **Stop** button stops the playback, not a render in progress:
  rendering is one native call on the engine thread, and a native `stop`
  would have to queue behind it. The host's `stop` export stays available to
  future live-audio wiring (`docs/building.md` records the AAudio story).
- The app renders with the bundled `bd` sample and synth voices only; the
  engine's network sample bank is not enabled behind the app's back
  (see `crates/android-host/src/lib.rs`).
