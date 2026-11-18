//! Rustel engine host for an Android app: evaluate a score, render it to
//! WAV bytes in memory, stop, collect diagnostics, and ask an
//! OpenAI-compatible model to rewrite the score.
//!
//! The crate is two layers. The core here is plain Rust with no Android
//! configuration, so it builds and its tests run on the host. The JNI shim
//! in [`jni`] is compiled only for `target_os = "android"` and exports the
//! core to Java; every other target skips it.
//!
//! QuickJS is thread-affine, so the core owns one session worker thread with
//! [`QUERY_WORKER_STACK_BYTES`] of stack and serialises every session call
//! onto it, the arrangement `docs/embedding.md` and the
//! `session_callback` example document. The transpiler spawns a 256 MiB
//! stack thread of its own for every parse; the worker's 64 MiB is smaller,
//! so the core does not wrap calls in `transpiler::on_caller_stack` - each
//! evaluate keeps that guard thread, which is the safe choice the embedding
//! guide leaves to hosts without a stack of their own.
//!
//! Scores render through [`Session::render_pcm`], which without
//! [`Session::enable_default_samples`] plays the bundled `bd` sample and
//! synth voices; the core leaves sample fetching off rather than opening
//! the network behind the app's back.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use rustel_engine::{QUERY_WORKER_STACK_BYTES, Session, SessionConfig, scheduler::Transport};

mod wav;

#[cfg(target_os = "android")]
mod jni;

/// The WAV file name [`AndroidHost::render_to_wav`] writes inside a caller's
/// directory: `<dir>/rustel-render.wav`.
pub const RENDER_WAV_NAME: &str = "rustel-render.wav";

/// One order for the session worker. Every answer travels on the reply
/// channel the order carries, so the public methods block exactly one call
/// at a time and the worker never shares the session.
enum Command {
    Evaluate {
        source: String,
        reply: Sender<Result<(), String>>,
    },
    RenderToWav {
        duration_secs: f64,
        out_dir: Option<PathBuf>,
        reply: Sender<Result<Vec<u8>, String>>,
    },
    Diagnostics {
        reply: Sender<Result<String, String>>,
    },
    Shutdown,
}

/// A headless Rustel session behind one worker thread.
///
/// Create with [`AndroidHost::new`], evaluate scores, render them to WAV
/// bytes, and drop the host to close it. The worker thread owns the
/// [`Session`]; every public method sends it one command and waits for the
/// answer, so no two calls interleave inside the engine.
pub struct AndroidHost {
    control: Sender<Command>,
    /// The transport handle taken before the worker starts answering, so
    /// [`Self::stop`] never queues behind a render in progress.
    transport: Arc<Transport>,
    worker: Option<JoinHandle<()>>,
}

impl AndroidHost {
    /// Start the session worker and build its session at the engine
    /// defaults: 48 kHz stereo, two seconds of QuickJS score budget. Returns
    /// the engine's message when the session cannot be built.
    pub fn new() -> Result<Self, String> {
        let (control, commands) = channel::<Command>();
        let (built, session_built) = channel::<Result<Arc<Transport>, String>>();
        let worker = std::thread::Builder::new()
            .name("rustel-android-host".into())
            .stack_size(QUERY_WORKER_STACK_BYTES)
            .spawn(move || session_worker(commands, built))
            .map_err(|error| format!("could not start the session worker: {error}"))?;
        let transport = session_built
            .recv()
            .map_err(|_| "the session worker exited before the session was built".to_string())??;
        Ok(Self {
            control,
            transport,
            worker: Some(worker),
        })
    }

    /// Transpile and evaluate a JavaScript score. A failed evaluation keeps
    /// the last good score installed and returns the engine's message. A
    /// successful evaluate restarts the transport, so a stop from an earlier
    /// render does not silence the next one.
    pub fn evaluate(&self, source: &str) -> Result<(), String> {
        self.ask(|reply| Command::Evaluate {
            source: source.to_owned(),
            reply,
        })
    }

    /// Render the installed score for `duration_secs` to WAV bytes:
    /// 16-bit PCM, interleaved at the session's channels and sample rate.
    ///
    /// With `out_dir`, an existing directory, also write those bytes to
    /// `out_dir/rustel-render.wav` so an app can hand a player a file. A
    /// [`Self::stop`] from another thread cancels the render, which returns
    /// the engine's cancellation message.
    pub fn render_to_wav(
        &self,
        duration_secs: f64,
        out_dir: Option<&Path>,
    ) -> Result<Vec<u8>, String> {
        self.ask(|reply| Command::RenderToWav {
            duration_secs,
            out_dir: out_dir.map(Path::to_owned),
            reply,
        })
    }

    /// Stop the transport immediately from the calling thread. A render in
    /// progress on the worker ends with the engine's cancellation error
    /// instead of running to its full length, because the transport is an
    /// atomic the render polls - stop must not queue behind the command
    /// channel. The next evaluate or render starts the transport again.
    pub fn stop(&self) {
        self.transport.stop();
    }

    /// Drain the session's diagnostics - `.log()` and `console.log` lines,
    /// voice resolver notices, plugin lines - as one JSON array of
    /// `{"kind", "message", "recoverable"}` objects. Taking them clears the
    /// queue, so an empty session answers `"[]"`.
    pub fn diagnostics_json(&self) -> Result<String, String> {
        self.ask(|reply| Command::Diagnostics { reply })
    }

    /// Rewrite `score` per `prompt` through one OpenAI-compatible chat call:
    /// `endpoint` is the base URL ahead of `/chat/completions`, `model` the
    /// model to name, `key` the caller's API key for the `Authorization`
    /// header. Returns the complete rewritten score, or the error's message,
    /// which names the kind of failure: HTTP status, transport, malformed
    /// JSON, or an empty reply.
    ///
    /// The rewrite never touches the session, so it runs on the calling
    /// thread and does not queue on the worker: an evaluate or render never
    /// waits behind a network call. The app still runs it on its engine
    /// thread like every other native call, one call at a time.
    pub fn llm_rewrite(
        &self,
        endpoint: &str,
        model: &str,
        key: &str,
        score: &str,
        prompt: &str,
    ) -> Result<String, String> {
        let endpoint = rustel_pod_llm::Endpoint::new(endpoint, model, key);
        endpoint
            .rewrite(
                rustel_pod_llm::STRUDEL_SYSTEM_PROMPT,
                &llm_user_message(score, prompt),
            )
            .map_err(|error| error.to_string())
    }

    /// Send `command` and wait for its answer. The worker is the only
    /// sender of replies, so a closed channel means it exited; the failure
    /// is reported, not hidden behind a default answer.
    fn ask<T>(
        &self,
        command: impl FnOnce(Sender<Result<T, String>>) -> Command,
    ) -> Result<T, String> {
        let (reply, answers) = channel();
        self.control
            .send(command(reply))
            .map_err(|_| "the session worker exited".to_string())?;
        match answers.recv() {
            Ok(answer) => answer,
            Err(_) => Err("the session worker exited mid-command".to_string()),
        }
    }
}

impl Drop for AndroidHost {
    fn drop(&mut self) {
        let _ = self.control.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            // A join error is a worker panic; the session died with it, and
            // there is no caller left to tell.
            let _ = worker.join();
        }
    }
}

/// The session worker: build the session, then answer commands until
/// shutdown. This thread is the only place the session is touched.
fn session_worker(
    commands: Receiver<Command>,
    session_built: Sender<Result<Arc<Transport>, String>>,
) {
    // The host keeps no copy of the config: `Session` does not expose the
    // one it was built with, and the worker needs the sample rate and
    // channel count to encode the WAV.
    let config = SessionConfig::default();
    let mut session = match Session::with_config(config.clone()) {
        Ok(session) => session,
        Err(error) => {
            // The channel is the only way out; the constructor learns of the
            // failure through its own recv.
            let _ = session_built.send(Err(error.to_string()));
            return;
        }
    };
    if session_built.send(Ok(session.transport())).is_err() {
        return;
    }
    while let Ok(command) = commands.recv() {
        match command {
            Command::Evaluate { source, reply } => {
                let _ = reply.send(session.evaluate(&source).map_err(|e| e.to_string()));
            }
            Command::RenderToWav {
                duration_secs,
                out_dir,
                reply,
            } => {
                let _ = reply.send(render_to_wav_bytes(
                    &mut session,
                    &config,
                    duration_secs,
                    out_dir,
                ));
            }
            Command::Diagnostics { reply } => {
                let _ = reply.send(diagnostics_json(&mut session));
            }
            Command::Shutdown => return,
        }
    }
}

/// Render `duration_secs` of the installed score to WAV bytes, and write
/// them to `out_dir`/[`RENDER_WAV_NAME`] when the caller gave a directory.
fn render_to_wav_bytes(
    session: &mut Session,
    config: &SessionConfig,
    duration_secs: f64,
    out_dir: Option<PathBuf>,
) -> Result<Vec<u8>, String> {
    let pcm = session
        .render_pcm(duration_secs)
        .map_err(|error| error.to_string())?;
    let wav = wav::pcm16_wav_bytes(config.sample_rate, config.channels, &pcm)?;
    if let Some(dir) = out_dir {
        let path = dir.join(RENDER_WAV_NAME);
        std::fs::write(&path, &wav)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }
    Ok(wav)
}

/// Serialise the session's pending diagnostics as one JSON array of
/// `{"kind", "message", "recoverable"}` objects.
fn diagnostics_json(session: &mut Session) -> Result<String, String> {
    let records: Vec<serde_json::Value> = session
        .take_diagnostics()
        .iter()
        .map(|diagnostic| {
            serde_json::json!({
                "kind": diagnostic.kind,
                "message": diagnostic.message,
                "recoverable": diagnostic.recoverable,
            })
        })
        .collect();
    serde_json::to_string(&records)
        .map_err(|error| format!("could not serialise the diagnostics: {error}"))
}

/// The user half of the rewrite request: the score as it stands, and the
/// change the prompt asks for. The system half is the pod-llm crate's
/// constant, which asks the model to reply with the complete rewritten
/// score alone. The studio's llm pod sends the same message.
fn llm_user_message(score: &str, prompt: &str) -> String {
    format!("The current score:\n\n{score}\n\nThe change: {prompt}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loudest 16-bit sample of the data chunk, skipping the 44-byte
    /// header `wav::pcm16_wav_bytes` writes.
    fn peak(wav: &[u8]) -> i16 {
        wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn a_score_renders_to_audible_wav_bytes() {
        let host = AndroidHost::new().expect("the session builds");
        host.evaluate(r#"note("c4 e4 g4 c5").s("sine").gain(0.2)"#)
            .expect("the score evaluates");
        let wav = host.render_to_wav(1.0, None).expect("the render completes");
        // One second of stereo 16-bit at 48 kHz, plus the header. The data
        // chunk length must agree with the byte count, not with what the
        // caller asked for.
        let data_bytes = u32::from_le_bytes(
            wav[40..44]
                .try_into()
                .expect("the data chunk length is four bytes"),
        ) as usize;
        assert_eq!(wav.len(), 44 + data_bytes);
        assert!(data_bytes > 47_000 * 4, "the render is too short to check");
        let loudest = peak(&wav);
        // The gain-0.2 score peaks near 2200 of 32767 after its envelope;
        // anything clearly above dither is audible, silence is the bug.
        assert!(
            loudest > 1_000,
            "the render should carry audible audio, peak {loudest}"
        );
    }

    #[test]
    fn a_render_with_a_directory_also_writes_the_wav_file() {
        let dir = std::env::temp_dir().join("rustel-android-host-render-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test directory is writable");
        let host = AndroidHost::new().expect("the session builds");
        host.evaluate(r#"note("c4 g4").s("sine").gain(0.2)"#)
            .expect("the score evaluates");
        let wav = host
            .render_to_wav(0.5, Some(&dir))
            .expect("the render completes");
        let written = std::fs::read(dir.join(RENDER_WAV_NAME)).expect("the wav file exists");
        assert_eq!(written, wav, "the file and the bytes must match");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn console_log_lines_are_captured_as_diagnostics_json() {
        let host = AndroidHost::new().expect("the session builds");
        host.evaluate(r#"console.log('hello from the score'); $: note("c4").s("sine")"#)
            .expect("the score evaluates");
        // The engine queues a log line when the note sounds, so a render
        // window has to pass before the diagnostic exists to drain.
        host.render_to_wav(0.1, None).expect("the render completes");
        let json = host.diagnostics_json().expect("the diagnostics collect");
        assert!(json.contains("\"kind\":\"log\""), "kind missing: {json}");
        assert!(
            json.contains("hello from the score"),
            "message missing: {json}"
        );
        // Taking the diagnostics clears them.
        let drained = host
            .diagnostics_json()
            .expect("an empty queue still answers");
        assert_eq!(drained, "[]");
    }

    #[test]
    fn a_broken_score_reports_its_error_and_the_host_keeps_working() {
        let host = AndroidHost::new().expect("the session builds");
        let error = host
            .evaluate("definitely not a ( score")
            .expect_err("the broken source is refused");
        assert!(!error.is_empty(), "the refusal carries a message");
        // The worker still answers after a failed evaluation.
        host.evaluate(r#"note("c4").s("sine")"#)
            .expect("the host still evaluates");
        host.render_to_wav(0.1, None)
            .expect("the host still renders");
    }

    /// Serves one canned HTTP reply on a background thread and hands back
    /// the base URL to call plus a handle that joins to the request the
    /// client sent, head and body. Modeled on the pod-llm crate's own
    /// mock server, so the rewrite can be proven on the host without a
    /// real endpoint.
    fn serve_once(response: String) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        let address = listener.local_addr().expect("read the listener's address");
        let server = std::thread::Builder::new()
            .name("mock-openai-server".into())
            .spawn(move || {
                let (mut stream, _) = listener.accept().expect("the client connects");
                let mut raw = Vec::new();
                let head_end = loop {
                    let mut chunk = [0u8; 4096];
                    let read = stream.read(&mut chunk).expect("read the request head");
                    assert!(read > 0, "the client closed before a full head arrived");
                    raw.extend_from_slice(&chunk[..read]);
                    if let Some(position) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break position + 4;
                    }
                };
                let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(name, value)| {
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().to_owned())
                        })
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                while raw.len() < head_end + content_length {
                    let mut chunk = [0u8; 4096];
                    let read = stream.read(&mut chunk).expect("read the request body");
                    assert!(read > 0, "the client closed before a full body arrived");
                    raw.extend_from_slice(&chunk[..read]);
                }
                stream
                    .write_all(response.as_bytes())
                    .expect("write the canned reply");
                String::from_utf8_lossy(&raw).into_owned()
            })
            .expect("spawn the mock server thread");
        (format!("http://{address}"), server)
    }

    /// A complete HTTP/1.1 reply with the given status line and body.
    fn http_reply(status_line: &str, body: &str) -> String {
        format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn an_llm_rewrite_returns_the_models_score() {
        let content = "console.log('rewritten by the mock');\n$: s(\"bd sd\")\n";
        let body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }],
        })
        .to_string();
        let (base, server) = serve_once(http_reply("HTTP/1.1 200 OK", &body));
        let host = AndroidHost::new().expect("the session builds");
        let score = host
            .llm_rewrite(
                &base,
                "test-model",
                "test-key",
                "$: s(\"bd\")",
                "add a snare",
            )
            .expect("the rewrite succeeds");
        assert_eq!(
            score,
            "console.log('rewritten by the mock');\n$: s(\"bd sd\")"
        );
        let request = server.join().expect("the mock server thread ends");
        assert!(
            request.contains("Authorization: Bearer test-key"),
            "the key travels in the bearer header: {request}"
        );
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .expect("the request has a body");
        let sent: serde_json::Value = serde_json::from_str(body).expect("the request body is JSON");
        assert_eq!(
            sent["messages"][0]["content"],
            rustel_pod_llm::STRUDEL_SYSTEM_PROMPT
        );
        assert_eq!(
            sent["messages"][1]["content"],
            "The current score:\n\n$: s(\"bd\")\n\nThe change: add a snare"
        );
    }

    #[test]
    fn a_failed_llm_rewrite_reports_its_kind_and_leaves_the_host_working() {
        let (base, server) = serve_once(http_reply("HTTP/1.1 500 Internal Server Error", "boom"));
        let host = AndroidHost::new().expect("the session builds");
        let error = host
            .llm_rewrite(
                &base,
                "test-model",
                "test-key",
                "$: s(\"bd\")",
                "add a snare",
            )
            .expect_err("a 500 fails the rewrite");
        server.join().expect("the mock server thread ends");
        assert!(
            error.contains("HTTP 500"),
            "the error names the status: {error}"
        );
        // The session never saw the failed call, and still answers.
        host.evaluate(r#"note("c4").s("sine")"#)
            .expect("the host still evaluates");
    }
}
