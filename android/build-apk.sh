#!/usr/bin/env bash
# Builds the Rustel Android APK without Gradle: cargo-ndk for the native
# library, then aapt2 + javac + d8 + zipalign + apksigner for the app.
#
# Usage: android/build-apk.sh [profile]
#   profile: debug | release (default: release)
#
# Requirements, with the paths this script looks for by default:
#   - Android SDK with build-tools 34.0.0 and platform android-34:
#       /usr/lib/android-sdk (override with ANDROID_SDK_ROOT)
#   - Android NDK r30: /home/erik/android-sdk/ndk/30.0.16248370
#       (override with ANDROID_NDK_HOME)
#   - cargo-ndk, and the rust targets aarch64-linux-android and
#     x86_64-linux-android (the latter is optional; the script skips it
#     when the target is not installed).
#   - JDK 17+ for javac, keytool and d8.
#
# The script is re-runnable: every output under android/build/ is rebuilt
# from scratch except the debug keystore, which is generated once so
# successive builds keep signing with the same key.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$REPO_ROOT/android"
OUT_DIR="$APP_DIR/build"

PROFILE="${1:-release}"
if [ "$PROFILE" = "release" ]; then
    CARGO_PROFILE_ARG="--release"
    CARGO_PROFILE_DIR="release"
elif [ "$PROFILE" = "debug" ]; then
    CARGO_PROFILE_ARG=""
    CARGO_PROFILE_DIR="debug"
else
    echo "unknown profile '$PROFILE' (expected debug or release)" >&2
    exit 2
fi

SDK="${ANDROID_SDK_ROOT:-/usr/lib/android-sdk}"
BUILD_TOOLS="$SDK/build-tools/34.0.0"
PLATFORM_JAR="$SDK/platforms/android-34/android.jar"
NDK="${ANDROID_NDK_HOME:-/home/erik/android-sdk/ndk/30.0.16248370}"
SYSROOT="$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
MIN_API=21

if [ ! -d "$BUILD_TOOLS" ]; then
    echo "build-tools not found at $BUILD_TOOLS (set ANDROID_SDK_ROOT)" >&2
    exit 2
fi
if [ ! -f "$PLATFORM_JAR" ]; then
    echo "platform jar not found at $PLATFORM_JAR (set ANDROID_SDK_ROOT)" >&2
    exit 2
fi

# The ABIs the APK carries. x86_64 exists for the emulator; skip it when the
# rust target is not installed.
ABIS=(aarch64-linux-android)
if rustup target list --installed 2>/dev/null | grep -q '^x86_64-linux-android$'; then
    ABIS+=(x86_64-linux-android)
fi

# 1. The native library. cargo-ndk exports an underscore-spelled
#    BINDGEN_EXTRA_CLANG_ARGS triple variable without a --target, and bindgen
#    would then derive an unversioned triple that bionic's sys/cdefs.h
#    rejects. The dash-spelled variable wins over it, so each target gets its
#    own with the versioned triple and the NDK sysroot
#    (see /home/erik/android-probe.md and docs/building.md).
echo "== cargo ndk: rustel-android-host ($PROFILE) =="
for abi in "${ABIS[@]}"; do
    echo "-- building $abi"
    env "ANDROID_NDK_HOME=$NDK" \
        "BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android=--target=aarch64-linux-android${MIN_API} --sysroot=$SYSROOT -I$SYSROOT/usr/include/aarch64-linux-android" \
        "BINDGEN_EXTRA_CLANG_ARGS_x86_64-linux-android=--target=x86_64-linux-android${MIN_API} --sysroot=$SYSROOT -I$SYSROOT/usr/include/x86_64-linux-android" \
        cargo ndk -t "$abi" build -p rustel-android-host $CARGO_PROFILE_ARG \
        --manifest-path "$REPO_ROOT/Cargo.toml"
done

# 2. Clean staging.
STAGE="$OUT_DIR/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE/lib" "$OUT_DIR/classes" "$OUT_DIR/dex" "$OUT_DIR/gen"

# 3. Resources and the manifest: aapt2 compiles and links them into the
#    unsigned APK skeleton and emits the R.java the Java sources use.
echo "== aapt2 link =="
"$BUILD_TOOLS/aapt2" compile --dir "$APP_DIR/res" -o "$OUT_DIR/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$OUT_DIR/unsigned.apk" \
    -I "$PLATFORM_JAR" \
    --manifest "$APP_DIR/AndroidManifest.xml" \
    --min-sdk-version "$MIN_API" \
    --version-code 1 --version-name 0.1.0 \
    --java "$OUT_DIR/gen" \
    "$OUT_DIR/res.zip"

# 4. Java sources: javac against the platform jar, then d8 into one dex.
#    --release 8 keeps the bytecode inside what every Android supports.
echo "== javac + d8 =="
javac --release 8 -Xlint:-options \
    -classpath "$PLATFORM_JAR" \
    -d "$OUT_DIR/classes" \
    "$OUT_DIR/gen/dev/rustel/android/R.java" \
    "$APP_DIR/java/dev/rustel/android/RustelHost.java" \
    "$APP_DIR/java/dev/rustel/android/MainActivity.java"

find "$OUT_DIR/classes" -name '*.class' > "$OUT_DIR/classes.txt"
# shellcheck disable=SC2046
"$BUILD_TOOLS/d8" --release --lib "$PLATFORM_JAR" --min-api "$MIN_API" \
    --output "$OUT_DIR/dex" $(cat "$OUT_DIR/classes.txt")

cp "$OUT_DIR/dex/classes.dex" "$STAGE/classes.dex"

# 5. The native libraries, at the APK paths the loader looks in
#    (lib/<abi>/librustel_android_host.so), where <abi> is the Android ABI
#    name, not the rust triple.
for abi in "${ABIS[@]}"; do
    case "$abi" in
        aarch64-linux-android) apk_abi=arm64-v8a ;;
        armv7-linux-androideabi) apk_abi=armeabi-v7a ;;
        i686-linux-android) apk_abi=x86 ;;
        x86_64-linux-android) apk_abi=x86_64 ;;
        *) apk_abi="$abi" ;;
    esac
    mkdir -p "$STAGE/lib/$apk_abi"
    cp "$REPO_ROOT/target/$abi/$CARGO_PROFILE_DIR/librustel_android_host.so" \
        "$STAGE/lib/$apk_abi/"
done

# 6. Pack the dex and the libraries into the APK skeleton.
echo "== pack =="
(cd "$STAGE" && zip -q -r "$OUT_DIR/unsigned.apk" classes.dex lib)

# 7. Align, sign, verify.
echo "== zipalign + apksigner =="
KEYSTORE="$OUT_DIR/debug.keystore"
if [ ! -f "$KEYSTORE" ]; then
    keytool -genkeypair -keystore "$KEYSTORE" \
        -storepass android -keypass android \
        -alias androiddebugkey \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -dname "CN=Android Debug,O=Android,C=US"
fi

"$BUILD_TOOLS/zipalign" -f 4 "$OUT_DIR/unsigned.apk" "$OUT_DIR/aligned.apk"
"$BUILD_TOOLS/apksigner" sign \
    --ks "$KEYSTORE" --ks-pass pass:android --key-pass pass:android \
    --out "$OUT_DIR/rustel.apk" "$OUT_DIR/aligned.apk"
"$BUILD_TOOLS/apksigner" verify --print-certs "$OUT_DIR/rustel.apk"

echo "== result =="
"$BUILD_TOOLS/aapt" dump badging "$OUT_DIR/rustel.apk" | head -n 5
ls -lh "$OUT_DIR/rustel.apk"
echo "APK: $OUT_DIR/rustel.apk"
