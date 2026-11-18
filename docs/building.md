# Build Rustel

The toolchain is pinned in `rust-toolchain.toml`. On Linux and macOS you
also need a C toolchain with `sh` and `make` on `PATH`. The MP3 encoder is
LAME, and its vendored source builds through its own `configure` script.
Windows needs neither `sh` nor `make`; its build uses only the compiler. The
default build is the complete product, including Studio and Hydra:

```sh
cargo build --profile local -p rustel
```

## Build profiles

Use `local` for an optimized build during development. It inherits the
`release` settings, including optimization level 3 and stripped symbols,
but uses thin link-time optimization (LTO), 16 code-generation units, and
incremental compilation. These settings favor faster rebuilds. The binary
can be larger than a release build; size and runtime speed depend on the
platform and code.

Use `release` for the final optimized binary. It uses fat LTO and one
code-generation unit for optimization across the whole program, at the
cost of longer builds. Both optimized profiles keep panic unwinding for
score recovery.

| Build command | Binary directory |
| --- | --- |
| `cargo build -p rustel` | `target/debug/` - the unoptimized `dev` profile, with debug information |
| `cargo build --profile local -p rustel` | `target/local/` |
| `cargo build --release -p rustel` | `target/release/` |

The executable is `rustel`, or `rustel.exe` on Windows. To build and launch
Studio in one command, use `cargo run --profile local -p rustel -- studio`.
Replace `--profile local` with `--release` to use the release profile.

## Build features and dependencies

A headless build for `query`, `check`, and offline `render` can leave the
default live/visual features out explicitly:

```sh
cargo build --profile local -p rustel --no-default-features --features mp3-export
```

Optional features, combinable as needed:

| Feature | Adds |
| --- | --- |
| `mp3-export` | MP3 export through LAME; included by default, independent of MP3 sample decoding |
| `device-audio` | live playback through CPAL - AAudio on Android, see [Android](#android-aaudio-cross-build) |
| `midi` | MIDI input and output |
| `gamepad` | native gamepad input, included by default; Linux builds require libudev headers |
| `serial` | serial output for microcontrollers, included by default |
| `studio` | the terminal editor, included by default (includes device audio and MIDI discovery) |
| `hydra` | [Hydra visuals](hydra.md), included by default and removable from a lean build |
| `themes` | the extra studio theme documents, included by default. A build without it keeps `rustel-dark` |
| `vst` | [VST3 plugins](plugins.md) for `.vst()` and `.vsti()`, included by default |

OSC output is included by default. On Debian and Ubuntu, the default and `--all-features`
builds need the C toolchain named above for the vendored mp3 encoder, ALSA headers
for live audio, libudev headers for gamepads, and `pkg-config` to locate those
libraries:

```sh
sudo apt-get install -y build-essential pkg-config libasound2-dev libudev-dev
```

To keep Studio, Hydra, and the other default features while omitting gamepad input:

```sh
cargo build --profile local -p rustel --no-default-features --features mp3-export,extensions,opus,osc,serial,hydra,themes,studio,remote-control,vst
```

Adding `--all-features` enables `gamepad` again and requires the libudev headers on Linux.

### ALSA on other distributions

The Linux audio and MIDI backends use ALSA. A PipeWire system does not
remove that build dependency: `pipewire-alsa` provides the running
connection, while the build still reads `alsa.pc`. When the headers are
missing, the `alsa-sys` build script stops with `Package alsa was not
found in the pkg-config search path`.

The package names differ by distribution:

| Distribution | Command |
| --- | --- |
| Debian, Ubuntu | `sudo apt-get install -y build-essential pkg-config libasound2-dev libudev-dev` |
| Fedora | `sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config alsa-lib-devel systemd-devel` |
| Arch | `sudo pacman -S --needed base-devel pkgconf alsa-lib systemd-libs` |
| openSUSE | `sudo zypper install -y gcc gcc-c++ make pkg-config alsa-devel systemd-devel` |
| Alpine | `sudo apk add build-base pkgconf alsa-lib-dev eudev-dev` |

Check the libraries with:

```sh
pkg-config --exists alsa && pkg-config --exists libudev && echo found
```

A headless build without `device-audio`, `midi` and `gamepad` needs neither
library. `rustelup --branch` and `--commit` run this check before they
build and print the command for the detected distribution.

## Android (AAudio) cross-build

Live output on Android runs through CPAL's AAudio backend. cpal 0.18.2
ships a real AAudio host for `target_os = "android"`, and it is the
default host there, so the `device-audio` feature needs no extra gate:
the device layer prefers PulseAudio only on Linux, and every other target
- Android included - opens CPAL's default host. The live callback, the
ring, meters and the insert-plugin pool are shared with the desktop
platforms unchanged.

Cross-compile with the NDK and `cargo-ndk`, both for `aarch64-linux-android`
(`arm64-v8a`). Every cross-build on this setup also needs the dash-spelled
`BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android` variable: bindgen reads it
before `cargo-ndk`'s underscore-spelled value, which names the NDK sysroot
but no `--target`, and without a versioned triple bionic's `sys/cdefs.h`
rejects the compile:

```sh
NDK=$HOME/android-sdk/ndk/<version>
SYSROOT=$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot
env \
  ANDROID_NDK_HOME=$NDK \
  "BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android=--target=aarch64-linux-android29 --sysroot=$SYSROOT -I$SYSROOT/usr/include/aarch64-linux-android" \
  cargo ndk -t aarch64-linux-android -P 29 \
  build -p rustel --no-default-features --features device-audio,midi
```

The `-P 29` API level is a link requirement of the live path: AAudio's
`libaaudio` first ships at API 26, and with `midi` enabled midir's AMidi
backend also links `libamidi`, which first ships at API 29. A live-path
binary without `midi` needs only the AAudio level, 26.

`device-audio,midi` is the lean feature combination for the command's
live path. `device-audio` without `midi` does not build the command on any
platform: the live command's Link timing reads the runtime's `midi_clock`
module, which the `midi` feature gates.

The studio cross-compiles with the same recipe and its full default
feature set. `arboard` has no Android platform module and winit wants
an `android_main` entrypoint inside a JVM-bearing app there, so both are
target-gated off on Android: the studio's clipboard keeps only its
process-local fallback (a copy stays useful inside the studio, a paste of
nothing copied there reports the empty clipboard), and the detached video
window is declined - the clip keeps playing in the dock's video widget,
with the reason in the status line.

Runtime behavior on Android, as far as the code shows (validated to the
link; no Android device was attached at the time of writing):

- A live stream opens only inside a JVM-bearing Android app. CPAL's AAudio
  output queries the platform's AudioManager over JNI after opening the
  stream, and device enumeration goes through JNI the same way; both call
  `ndk_context::android_context()`, which panics in a process that has
  not initialized it. A bare `adb shell` or Termux binary therefore
  cannot open live output. Live output needs a host that initializes
  ndk-context with the app's JavaVM and Activity context before
  playback; `rustel-android-host` does not provide this hook yet, and
  the app under `android/` renders offline.
- Without a JNI device listing, CPAL falls back to a single unnamed
  default device, so `--output` with a device name has nothing to match.
- The callback runs with the platform's ordinary scheduling. No plugin
  worker threads start on Android - the worker pool reads the system's
  performance cores, which Android does not report - so insert plugins
  render on the audio callback, and no thread asks for a real-time class.
- The default 128-frame live buffer reaches AAudio as
  `frames_per_data_callback = 128` with a 256-frame capacity. CPAL's own
  underrun-driven buffer growth stays off because the request is fixed,
  and CPAL's `realtime` feature, which would ask AAudio for its
  low-latency performance mode explicitly, is not enabled.
- Device loss is not recovered on any platform, and Android disconnects
  AAudio streams on route changes such as headphones or Bluetooth: the
  failure is reported through the live path's error reporting rather than
  reopened. Xrun counts come from AAudio and are reported the same way as
  on the desktop.
- CPAL's AAudio host provides input streams and Rustel's input path
  compiles for Android, but the same JNI requirement applies, and input
  is untested on hardware.

The `android/` directory holds the app that embeds the engine today: a
minimal single-activity APK that evaluates a score and renders it to WAV
through `rustel-android-host`, built without Gradle by
`android/build-apk.sh`. See [the app's README](../android/README.md) for
the build steps and the device requirements.

Visuals need no separate system graphics library. Procedural and camera
visuals need no network, while `initImage()`
fetches the public HTTPS image named by the score. Check your build with
`rustel --version` or `rustel doctor`.

See [Contributing](../CONTRIBUTING.md) for development checks and CI.
