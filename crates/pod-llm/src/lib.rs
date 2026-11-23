//! The pod LLM client: one OpenAI-compatible chat call that rewrites a
//! Rustel score.
//!
//! Mode A of the pod design. The studio and the Android host both hand an
//! [`Endpoint`] the current score plus the requested change, and receive the
//! complete rewritten score. The call is single-shot and non-streaming: one
//! `POST /chat/completions` against the caller's base URL, bounded by a
//! sixty-second timeout, and the reply's assistant text is returned after
//! [`clean_score`] strips a stray code fence and refuses empty output.
//!
//! The crate holds no key of its own: [`Endpoint`] carries the caller's key
//! and sends it only in the `Authorization: Bearer` header of the request.
//!
//! [`system_prompt`] assembles the other half of the request - the
//! instructions, the language reference this build installs, and any
//! documents the player added - as one block that does not change between
//! calls.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use rustel_runtime::reference::{SheetDetail, cheat_sheet};
use serde::Deserialize;

/// The system prompt for a Strudel score rewrite: the instructions alone.
///
/// The contract it asks the model for, and [`clean_score`] enforces on the
/// way back: the reply is the complete rewritten score alone, with no code
/// fence and no commentary. [`system_prompt`] is this text plus the
/// language reference; a caller that wants no reference at all passes this
/// constant as the `system` half of [`Endpoint::rewrite`]. The `user` half
/// names the current score and the requested change.
pub const STRUDEL_SYSTEM_PROMPT: &str = "\
You rewrite Rustel score files. Rustel is a native Rust audio engine for
the Strudel pattern language: a score is JavaScript that builds patterns,
for example $: s(\"bd [sd hh] cp\"). The user message names the current
score and the requested change.

Reply with the complete rewritten score and nothing else: no markdown code
fences, no commentary, no explanation of the edit. The reply must be a
full, valid score on its own, not a fragment or a diff. Where the request
does not ask for a change, keep the current score's style, structure and
wording.";

/// How much of the language reference the system message carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ContextSize {
    /// One line per name: its signature, its aliases and its one-line
    /// documentation. About 69 kB - about 20k tokens, since this sheet runs
    /// near 3.5 bytes a token - on the complete product build;
    /// `docs/llm-reference-evidence.md` has the measurements.
    #[default]
    Compact,
    /// The same lines, then every parameter's type and description - where a
    /// default is read. About 141 kB, about 39k tokens, on the same build.
    Full,
}

impl ContextSize {
    /// The detail the shared renderer prints for this size.
    fn detail(self) -> SheetDetail {
        match self {
            Self::Compact => SheetDetail::Line,
            Self::Full => SheetDetail::Params,
        }
    }

    /// The one line that separates the instructions from the reference, and
    /// says what the list after it is. A build prints the same words every
    /// time, which is what keeps the prefix cacheable.
    fn header(self) -> &'static str {
        match self {
            Self::Compact => {
                "Reference: the names this Rustel build installs, one line each, \
                 with the signature and a one-line description:"
            }
            Self::Full => {
                "Reference: the names this Rustel build installs, one line each, \
                 with the signature, a one-line description and every parameter:"
            }
        }
    }
}

/// The whole system message for one rewrite: [`STRUDEL_SYSTEM_PROMPT`], then
/// the language reference for `size` when there is one, then `documents` -
/// the player's own files, verbatim, in the order given.
///
/// Everything here is a function of the build and of these two arguments,
/// never of the call: no clock, no paths, no score, no prompt. That is what
/// makes the message a cacheable prefix. Providers cache the longest prefix
/// of a request that repeats an earlier one, and bill the cached rest at a
/// fraction of the input price; a prefix that changes between calls is a
/// cache that never hits, so all the per-call material - the current score
/// and the requested change - belongs in the user message, after this one.
///
/// `None` leaves the reference block out and an empty `documents` adds
/// nothing, so either absence leaves the rest of the message as it was.
pub fn system_prompt(size: Option<ContextSize>, documents: &[String]) -> String {
    let mut prompt = STRUDEL_SYSTEM_PROMPT.to_owned();
    if let Some(size) = size {
        prompt.push_str("\n\n");
        prompt.push_str(size.header());
        prompt.push_str("\n\n");
        prompt.push_str(&cheat_sheet(size.detail()));
    }
    for document in documents {
        prompt.push_str("\n\n");
        prompt.push_str(document);
    }
    prompt
}

/// One rewrite gets at most this long: connect, send, and read combined.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The `max_tokens` sent with every request. Scores are small; this leaves
/// room for a long score without paying for an unbounded reply.
const MAX_TOKENS: u32 = 2048;

/// A reply body is read at most this far, so a broken endpoint cannot grow
/// the caller's memory without bound. A chat reply for a score is far
/// smaller.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// The first bytes of an error body kept for diagnostics.
const MAX_ERROR_BODY_BYTES: u64 = 512;

/// Why a rewrite failed.
#[derive(Debug)]
pub enum PodLlmError {
    /// The endpoint answered with an HTTP error status. `body` carries the
    /// first bytes of its reply, for the caller's diagnostics.
    Http { status: u16, body: String },
    /// The request never completed: DNS, connect, TLS, or the timeout.
    Transport(String),
    /// The endpoint answered, but the body is not the chat JSON it promises.
    MalformedJson(String),
    /// The body parsed, but it carries no assistant text: `choices` is empty
    /// or its first entry has no message content.
    EmptyChoice,
    /// The assistant text, once cleaned, is empty or blank.
    EmptyScore,
}

impl fmt::Display for PodLlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, body } => {
                write!(f, "the endpoint answered HTTP {status}: {body}")
            }
            Self::Transport(message) => write!(f, "the request failed: {message}"),
            Self::MalformedJson(message) => {
                write!(f, "the reply is not valid chat JSON: {message}")
            }
            Self::EmptyChoice => {
                write!(f, "the reply carries no assistant text: choices is empty")
            }
            Self::EmptyScore => write!(f, "the model's reply is empty after cleanup"),
        }
    }
}

impl std::error::Error for PodLlmError {}

/// One OpenAI-compatible chat endpoint and the credentials to call it.
///
/// The base URL is the part ahead of `/chat/completions`, for example
/// `https://api.openai.com/v1` or a local server's `http://127.0.0.1:8080/v1`.
/// The API key travels only in the `Authorization: Bearer` header of the
/// request; the crate sends it nowhere else.
pub struct Endpoint {
    base_url: String,
    model: String,
    temperature: Option<f64>,
    api_key: String,
}

impl Endpoint {
    /// Describes one endpoint: where to call, which model to name, and the
    /// caller's API key for the `Authorization` header.
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            temperature: None,
            api_key: api_key.into(),
        }
    }

    /// Sends this sampling temperature with the request. Without a call to
    /// this method the parameter is left out and the endpoint applies its
    /// own default.
    pub fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Rewrites a score in one call: `system` is the instruction
    /// ([`system_prompt`] for a Strudel rewrite, or [`STRUDEL_SYSTEM_PROMPT`]
    /// for the instructions alone), `user` names the current score and the
    /// requested change. Returns the cleaned assistant text, a complete
    /// score.
    pub fn rewrite(&self, system: &str, user: &str) -> Result<String, PodLlmError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        // ureq's default connect timeout is thirty seconds; set both ends of
        // the call to the same bound so no phase waits longer than the whole.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(REQUEST_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build();
        let response = agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(self.request_body(system, user))
            .map_err(ureq_error)?;
        let body = response.into_reader().take(MAX_RESPONSE_BYTES);
        let reply: ChatReply = serde_json::from_reader(body)
            .map_err(|error| PodLlmError::MalformedJson(error.to_string()))?;
        let assistant_text = reply.assistant_text().ok_or(PodLlmError::EmptyChoice)?;
        clean_score(&assistant_text)
    }

    /// The JSON body of one chat request: the model, the system and user
    /// messages, a bounded `max_tokens`, and the optional temperature. The
    /// request never streams.
    fn request_body(&self, system: &str, user: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "max_tokens": MAX_TOKENS,
            "stream": false,
        });
        if let Some(temperature) = self.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        body
    }
}

/// The chat JSON the endpoint is expected to answer with. Unknown fields are
/// ignored, so extra provider fields never fail a parse.
#[derive(Deserialize)]
struct ChatReply {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

impl ChatReply {
    /// The first choice's assistant text, or `None` when the reply carries
    /// none: an empty `choices` array, a choice without a message, or a
    /// message without content.
    fn assistant_text(self) -> Option<String> {
        self.choices.into_iter().next()?.message?.content
    }
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

/// Maps a ureq failure onto this crate's error: an error status keeps the
/// status and the head of its body; a transport fault keeps its message.
fn ureq_error(error: ureq::Error) -> PodLlmError {
    match error {
        ureq::Error::Status(status, response) => {
            // Bytes, then lossy text: the head can cut a multi-byte
            // character in the middle, and an error body owes the caller
            // diagnostics no encoding contract at all. Either way the
            // head is kept, as the `Http` variant's doc promises.
            let mut head = Vec::new();
            let _ = response
                .into_reader()
                .take(MAX_ERROR_BODY_BYTES)
                .read_to_end(&mut head);
            let body = String::from_utf8_lossy(&head).into_owned();
            PodLlmError::Http { status, body }
        }
        ureq::Error::Transport(transport) => PodLlmError::Transport(transport.to_string()),
    }
}

/// Cleans the assistant's reply into score text: trims surrounding
/// whitespace, removes a surrounding markdown code fence the model added
/// despite the prompt, and refuses an empty result.
pub fn clean_score(reply: &str) -> Result<String, PodLlmError> {
    let trimmed = reply.trim();
    let score = strip_code_fence(trimmed).unwrap_or(trimmed);
    if score.is_empty() {
        return Err(PodLlmError::EmptyScore);
    }
    Ok(score.to_owned())
}

/// Removes one surrounding ``` fence, with its optional language tag, when
/// `text` opens with a fence line and closes with one. Returns the inner
/// text trimmed. Returns `None` for anything else, leaving the caller the
/// text as it stands.
fn strip_code_fence(text: &str) -> Option<&str> {
    let after_open = text.strip_prefix("```")?;
    // The opening fence may name a language: ```strudel. Whatever the line
    // holds, the score starts on the next one.
    let (_, body) = after_open.split_once('\n')?;
    let body = body.strip_suffix("```")?;
    Some(body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;

    /// Serves one HTTP request on a background thread and answers it with
    /// `response`, a complete HTTP/1.1 reply as raw bytes, so a test can
    /// also serve an error body that is not UTF-8. Returns the base URL
    /// to call and a handle that joins to the request the client sent,
    /// head and body, for header and body assertions.
    fn serve_once(response: impl Into<Vec<u8>>) -> (String, JoinHandle<String>) {
        // Bytes, so a test can also serve an error body that is not
        // UTF-8; the closure takes the reply already converted.
        let response = response.into();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        let address = listener.local_addr().expect("read the listener's address");
        let server = std::thread::Builder::new()
            .name("mock-openai-server".into())
            .spawn(move || {
                let (mut stream, _) = listener.accept().expect("the client connects");
                let request = read_request(&mut stream);
                stream.write_all(&response).expect("write the canned reply");
                request
            })
            .expect("spawn the mock server thread");
        (format!("http://{address}"), server)
    }

    /// Reads one request: the head, then exactly the body its `Content-Length`
    /// names, so the server never waits for a close the keep-alive client
    /// never sends. Returns the whole request as text for assertions.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
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
            let read = stream.read(&mut chunk).expect("read the request body");
            assert!(read > 0, "the client closed before a full body arrived");
            raw.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&raw).into_owned()
    }

    /// A canned 200 OK whose first choice says `content`.
    fn openai_reply(content: &str) -> String {
        let body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }],
        });
        http_reply("HTTP/1.1 200 OK", &body.to_string())
    }

    /// Wraps `body` in a complete HTTP/1.1 reply with the given status line.
    fn http_reply(status_line: &str, body: &str) -> String {
        format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn the_request_carries_the_model_messages_max_tokens_and_the_temperature() {
        let endpoint =
            Endpoint::new("http://127.0.0.1:9", "test-model", "key").with_temperature(0.2);
        let body = endpoint.request_body(STRUDEL_SYSTEM_PROMPT, "$: s(\"bd\")");
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_tokens"], serde_json::json!(MAX_TOKENS));
        assert_eq!(body["stream"], false);
        assert_eq!(body["temperature"], 0.2);
        let messages = body["messages"]
            .as_array()
            .expect("messages is an array")
            .clone();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], STRUDEL_SYSTEM_PROMPT);
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "$: s(\"bd\")");
    }

    #[test]
    fn the_request_without_a_temperature_leaves_the_parameter_out() {
        let endpoint = Endpoint::new("http://127.0.0.1:9", "test-model", "key");
        let body = endpoint.request_body("system text", "user text");
        assert!(body.get("temperature").is_none());
    }

    /// The cache invariant: the same settings give the same bytes, so the
    /// prefix a provider cached for one call is the prefix of the next.
    #[test]
    fn the_system_prompt_is_the_same_bytes_on_every_call() {
        for size in [ContextSize::Compact, ContextSize::Full] {
            assert_eq!(
                system_prompt(Some(size), &[]),
                system_prompt(Some(size), &[]),
                "{size:?} is a function of the build alone"
            );
        }
        // Without a reference and without documents the message is the
        // instructions, byte for byte.
        assert_eq!(system_prompt(None, &[]), STRUDEL_SYSTEM_PROMPT);
    }

    /// The two sizes are the two sheets: the instructions first, then the
    /// reference, one line per name against every parameter too.
    #[test]
    fn both_sizes_open_with_the_instructions_and_differ() {
        let compact = system_prompt(Some(ContextSize::Compact), &[]);
        let full = system_prompt(Some(ContextSize::Full), &[]);
        for prompt in [&compact, &full] {
            assert!(
                prompt.starts_with(STRUDEL_SYSTEM_PROMPT),
                "the instructions lead: {}",
                &prompt[..80.min(prompt.len())]
            );
            assert!(
                prompt.contains("- `lpf(frequency)`"),
                "a name every build of the engine installs is in the reference"
            );
        }
        assert_ne!(compact, full, "the sizes are different sheets");
        assert_eq!(ContextSize::default(), ContextSize::Compact);
        assert!(
            full.len() > compact.len(),
            "the full sheet carries more: {} against {}",
            full.len(),
            compact.len()
        );
        assert!(
            full.matches("\n  - `").count() > 0 && !compact.contains("\n  - `"),
            "only the full sheet prints a parameter line for each entry"
        );
    }

    /// A document - a player's own reference file - lands after the sheet,
    /// and its absence leaves every earlier byte as it was.
    #[test]
    fn a_document_lands_after_the_sheet_and_changes_nothing_else() {
        let documents = ["The house style: two bars, then a break.".to_owned()];
        let without = system_prompt(Some(ContextSize::Compact), &[]);
        let with = system_prompt(Some(ContextSize::Compact), &documents);
        assert!(
            with.starts_with(&without),
            "the block before the document is untouched"
        );
        assert_eq!(with[without.len()..].trim_start(), documents[0]);
        assert!(
            with.find(&documents[0]) > with.find("## "),
            "the document follows the sheet's sections"
        );
    }

    /// The whole request as the endpoint sees it: the system half opens with
    /// the instructions and carries the reference, and the user half keeps
    /// only the score and the change.
    #[test]
    fn the_request_carries_the_reference_and_the_user_turn_carries_the_score() {
        let system = system_prompt(Some(ContextSize::Compact), &[]);
        let (base, server) = serve_once(openai_reply("$: s(\"bd sd\")\n"));
        let score = Endpoint::new(base, "test-model", "test-key")
            .rewrite(
                &system,
                "The current score:\n\n$: s(\"bd\")\n\nThe change: four",
            )
            .expect("the rewrite succeeds");
        assert_eq!(score, "$: s(\"bd sd\")");
        let request = server.join().expect("the mock server thread ends");
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .expect("the request has a body");
        let sent: serde_json::Value = serde_json::from_str(body).expect("the request body is JSON");
        let sent_system = sent["messages"][0]["content"]
            .as_str()
            .expect("the system half is text");
        assert!(sent_system.starts_with(STRUDEL_SYSTEM_PROMPT));
        assert!(sent_system.contains("- `lpf(frequency)`"));
        let sent_user = sent["messages"][1]["content"]
            .as_str()
            .expect("the user half is text");
        assert_eq!(
            sent_user, "The current score:\n\n$: s(\"bd\")\n\nThe change: four",
            "the score and the change, in that order"
        );
        assert!(
            !sent_user.contains("Reference:") && !sent_user.contains("lpf"),
            "no part of the documentation travels in the user turn"
        );
    }

    #[test]
    fn rewrite_returns_the_cleaned_score_and_sends_the_bearer_key() {
        let (base, server) = serve_once(openai_reply("$: s(\"bd sd\")\n"));
        let score = Endpoint::new(base, "test-model", "test-key")
            .rewrite("system text", "$: s(\"bd\") make it four")
            .expect("the rewrite succeeds");
        assert_eq!(score, "$: s(\"bd sd\")");
        let request = server.join().expect("the mock server thread ends");
        assert!(
            request.starts_with("POST /chat/completions HTTP/1.1"),
            "the request went to the chat completions path: {request}"
        );
        assert!(
            request.contains("Authorization: Bearer test-key"),
            "the key travels in the bearer header: {request}"
        );
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .expect("the request has a body");
        let sent: serde_json::Value = serde_json::from_str(body).expect("the request body is JSON");
        assert_eq!(sent["messages"][0]["content"], "system text");
        assert_eq!(sent["messages"][1]["content"], "$: s(\"bd\") make it four");
    }

    #[test]
    fn rewrite_strips_a_code_fence_the_model_added() {
        let fenced = "```strudel\n$: s(\"bd\")\n```\n";
        let (base, server) = serve_once(openai_reply(fenced));
        let score = Endpoint::new(base, "test-model", "test-key")
            .rewrite(STRUDEL_SYSTEM_PROMPT, "user text")
            .expect("the rewrite succeeds");
        server.join().expect("the mock server thread ends");
        assert_eq!(score, "$: s(\"bd\")");
    }

    #[test]
    fn a_base_url_with_a_trailing_slash_still_hits_one_path() {
        let (base, server) = serve_once(openai_reply("$: s(\"bd\")"));
        let score = Endpoint::new(format!("{base}/"), "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect("the rewrite succeeds");
        server.join().expect("the mock server thread ends");
        assert_eq!(score, "$: s(\"bd\")");
    }

    #[test]
    fn an_http_error_status_is_reported_with_the_status_and_body() {
        let (base, server) = serve_once(http_reply("HTTP/1.1 500 Internal Server Error", "boom"));
        let error = Endpoint::new(base, "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect_err("a 500 fails the rewrite");
        server.join().expect("the mock server thread ends");
        assert!(
            matches!(error, PodLlmError::Http { status: 500, ref body } if body == "boom"),
            "the error names the status: {error:?}"
        );
    }

    #[test]
    fn an_error_body_that_is_not_utf8_still_keeps_its_head() {
        // An error body owes the caller no encoding, and the 512-byte
        // head can cut a multi-byte character in half: a Latin-1 byte
        // and a truncated four-byte sequence must both survive as
        // lossy text, not be dropped whole by a failed UTF-8 read.
        let error_body: &[u8] = b"caf\xe9 \xf0\x9f";
        let mut reply = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            error_body.len()
        )
        .into_bytes();
        reply.extend_from_slice(error_body);
        let (base, server) = serve_once(reply);
        let error = Endpoint::new(base, "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect_err("a 500 fails the rewrite");
        server.join().expect("the mock server thread ends");
        assert!(
            matches!(
                error,
                PodLlmError::Http { status: 500, ref body }
                    if body == "caf\u{fffd} \u{fffd}"
            ),
            "the head is kept, lossily spelled: {error:?}"
        );
    }

    #[test]
    fn a_reply_that_is_not_json_is_reported_as_malformed() {
        let (base, server) = serve_once(http_reply("HTTP/1.1 200 OK", "not json at all"));
        let error = Endpoint::new(base, "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect_err("a non-JSON body fails the rewrite");
        server.join().expect("the mock server thread ends");
        assert!(
            matches!(error, PodLlmError::MalformedJson(_)),
            "the error names the parse: {error:?}"
        );
    }

    #[test]
    fn an_empty_choices_array_is_reported_as_an_empty_choice() {
        let (base, server) = serve_once(http_reply("HTTP/1.1 200 OK", "{\"choices\":[]}"));
        let error = Endpoint::new(base, "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect_err("an empty choices array fails the rewrite");
        server.join().expect("the mock server thread ends");
        assert!(
            matches!(error, PodLlmError::EmptyChoice),
            "the error names the empty choice: {error:?}"
        );
    }

    #[test]
    fn a_choice_without_content_is_reported_as_an_empty_choice() {
        let body = "{\"choices\":[{\"message\":{\"role\":\"assistant\"}}]}";
        let (base, server) = serve_once(http_reply("HTTP/1.1 200 OK", body));
        let error = Endpoint::new(base, "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect_err("a choice without content fails the rewrite");
        server.join().expect("the mock server thread ends");
        assert!(
            matches!(error, PodLlmError::EmptyChoice),
            "the error names the empty choice: {error:?}"
        );
    }

    #[test]
    fn a_blank_reply_is_refused() {
        let (base, server) = serve_once(openai_reply("   \n\t\n"));
        let error = Endpoint::new(base, "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect_err("a blank reply fails the rewrite");
        server.join().expect("the mock server thread ends");
        assert!(
            matches!(error, PodLlmError::EmptyScore),
            "the error names the empty score: {error:?}"
        );
    }

    #[test]
    fn a_refused_connection_is_reported_as_transport() {
        let error = Endpoint::new("http://127.0.0.1:1", "test-model", "test-key")
            .rewrite("system text", "user text")
            .expect_err("a refused connection fails the rewrite");
        assert!(
            matches!(error, PodLlmError::Transport(_)),
            "the error names the transport fault: {error:?}"
        );
    }

    fn clean_expect(reply: &str) -> String {
        clean_score(reply).expect("the reply cleans to a score")
    }

    #[test]
    fn clean_score_keeps_plain_and_inner_fences() {
        assert_eq!(clean_expect("  $: s(\"bd\")  \n"), "$: s(\"bd\")");
        assert_eq!(
            clean_expect("```\n$: s(\"bd\")\n```"),
            "$: s(\"bd\")",
            "a fence without a language tag is stripped too"
        );
        assert_eq!(
            clean_expect("$: s(\"bd\")\n$: s(\"sd\")"),
            "$: s(\"bd\")\n$: s(\"sd\")",
            "text without a fence stands as it is"
        );
    }

    #[test]
    fn clean_score_refuses_empty_and_fence_only_replies() {
        assert!(matches!(clean_score(""), Err(PodLlmError::EmptyScore)));
        assert!(matches!(clean_score(" \n\t"), Err(PodLlmError::EmptyScore)));
        assert!(matches!(
            clean_score("```strudel\n   \n```"),
            Err(PodLlmError::EmptyScore)
        ));
    }
}
