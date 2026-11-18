//! JNI exports of [`AndroidHost`] for the Android app. Compiled only for
//! `target_os = "android"`; every other target builds the core alone.
//!
//! The Java-side contract, which the app's Kotlin declares as:
//!
//! ```java
//! package dev.rustel.android;
//!
//! public final class RustelHost {
//!     static { System.loadLibrary("rustel_android_host"); }
//!
//!     /** Opens a session; the handle is its native address. Throws on
//!         failure and returns 0. */
//!     public static native long newSession();
//!     /** Evaluates a score source. Throws on a failed evaluation. */
//!     public static native void evaluate(long handle, String source);
//!     /** Renders `durationSecs` of the installed score to a WAV byte
//!         array; a non-null `outDir` also receives the bytes as
//!         `outDir/rustel-render.wav`. */
//!     public static native byte[] renderToWav(long handle, double durationSecs,
//!                                              String outDir);
//!     /** Stops the transport immediately; cancels a render in progress. */
//!     public static native void stop(long handle);
//!     /** Drains pending diagnostics as a JSON array string. */
//!     public static native String diagnosticsJson(long handle);
//!     /** Closes the session and frees the handle. */
//!     public static native void dispose(long handle);
//! }
//! ```
//!
//! Every method that takes a handle throws `java.lang.RuntimeException` with
//! the engine's message when the call fails, and the handle stays valid
//! afterwards except after `dispose`. `outDir` may be null; nothing else may
//! be.

use std::path::PathBuf;

use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::{jbyteArray, jdouble, jlong, jstring},
};

use crate::AndroidHost;

/// Borrow the host a handle names, or throw and answer nothing. A zero or
/// stale handle is a Java-side bug; the exports report it as an exception
/// instead of crashing the process.
fn host<'host>(env: &mut JNIEnv<'_>, handle: jlong) -> Option<&'host mut AndroidHost> {
    if handle == 0 {
        throw(
            env,
            "the session handle is null; newSession was not called or failed",
        );
        return None;
    }
    // SAFETY: `newSession` hands Java the address of a leaked box, and the
    // handle names that box until `dispose` takes it back. Each export's
    // borrow ends before it returns; the caller must not dispose a handle
    // while another call on it is still running, the same way it must not
    // use a handle after dispose.
    Some(unsafe { &mut *(handle as *mut AndroidHost) })
}

/// Throw the engine's message. `throw_new` cannot report its own failure
/// without an environment to throw with, so its result is dropped.
fn throw(env: &mut JNIEnv, message: &str) {
    let _ = env.throw_new("java/lang/RuntimeException", message);
}

/// Read a Java string argument into owned text, or throw and answer nothing.
fn rust_string<'local>(
    env: &mut JNIEnv<'local>,
    argument: JString<'local>,
    what: &str,
) -> Option<String> {
    match env.get_string(&argument) {
        Ok(text) => Some(String::from(text)),
        Err(_) => {
            throw(
                env,
                &format!("the {what} argument is not a valid Java string"),
            );
            None
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_rustel_android_RustelHost_newSession<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jlong {
    match AndroidHost::new() {
        Ok(host) => Box::into_raw(Box::new(host)) as jlong,
        Err(error) => {
            throw(&mut env, &error);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_rustel_android_RustelHost_evaluate<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    source: JString<'local>,
) {
    let Some(host) = host(&mut env, handle) else {
        return;
    };
    let Some(source) = rust_string(&mut env, source, "score source") else {
        return;
    };
    if let Err(error) = host.evaluate(&source) {
        throw(&mut env, &error);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_rustel_android_RustelHost_renderToWav<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    duration_secs: jdouble,
    out_dir: JString<'local>,
) -> jbyteArray {
    let Some(host) = host(&mut env, handle) else {
        return std::ptr::null_mut();
    };
    // A null `outDir` is the in-memory-only spelling, not an error.
    let out_dir = if out_dir.as_raw().is_null() {
        None
    } else {
        rust_string(&mut env, out_dir, "output directory").map(PathBuf::from)
    };
    let Some(out_dir) = out_dir else {
        return std::ptr::null_mut();
    };
    match host.render_to_wav(duration_secs, Some(out_dir.as_path())) {
        Ok(wav) => match env.byte_array_from_slice(&wav) {
            Ok(array) => array.into_raw(),
            Err(_) => {
                throw(
                    &mut env,
                    "the Java heap refused the WAV byte array allocation",
                );
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            throw(&mut env, &error);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_rustel_android_RustelHost_stop<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    if let Some(host) = host(&mut env, handle) {
        host.stop();
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_rustel_android_RustelHost_diagnosticsJson<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jstring {
    let Some(host) = host(&mut env, handle) else {
        return std::ptr::null_mut();
    };
    match host.diagnostics_json() {
        Ok(json) => match env.new_string(json) {
            Ok(string) => string.into_raw(),
            Err(_) => {
                throw(&mut env, "the Java heap refused the diagnostics string");
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            throw(&mut env, &error);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_rustel_android_RustelHost_dispose<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    // SAFETY: `newSession` leaked the box with `Box::into_raw` and Java
    // hands the address to `dispose` exactly once; taking the box back here
    // is that transfer of ownership, and the drop joins the worker.
    drop(unsafe { Box::from_raw(handle as *mut AndroidHost) });
}
