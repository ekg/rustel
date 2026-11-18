//! The pod prompt, and the pods it starts: what the player configures to
//! take a prompt and rewrite the score.
//!
//! Which pod answers is the pod kind row in Settings ▸ Live coding:
//!
//! - `command` runs the command template below it - an AI agent CLI, a
//!   script of one's own, anything at all.
//! - `llm` makes one chat call to an OpenAI-compatible endpoint (the
//!   `rustel-pod-llm` crate) with the active tab's score and the prompt,
//!   and the reply replaces the score.
//! - `agent` runs a persistent agent process: the command line the
//!   `agent command` row names (the default is `pi --mode rpc
//!   --no-session`), speaking the RPC protocol - one JSON object per
//!   line, commands on its stdin, responses and events on its stdout.
//!   One process stays alive across prompts, which is the whole point:
//!   every prompt lands in the same conversation, so the agent keeps
//!   the context it built - the score, the earlier changes, what it
//!   said about them. A prompt is one `prompt` command under an id of
//!   its own; the agent's text deltas and tool narration stream into
//!   the log while it works, and the prompt is done when the agent
//!   settles.
//!
//! The command pod is any command that takes a prompt and writes score
//! files. The template - `my-agent --prompt {prompt} {file}` - runs with
//! the set folder as its working directory, so a pod that writes beside
//! itself lands in the set. `{set}` and `{file}` are handed as absolute
//! paths, so they name the same folders and files whatever directory the
//! pod reads them from. No shell stands between the studio and the
//! program: the template is split on whitespace into `Command`
//! arguments, and nothing in it is quoted, escaped or expanded by
//! anything else.
//!
//! What the command pod writes, the [external changes
//! watch](external_changes.rs) follows: `sync` takes the saved text into
//! the editor, and `sync & eval` plays it. The agent pod writes score
//! files the same way - with its own file tools, from the set folder it
//! runs in - so the same watch (or the player's own update key) applies
//! what it saves; the pane owns no copy of the agent's answer. The llm
//! pod needs no watch: the studio itself wrote what it adopted, so the
//! reply goes into the tab it came from, its file takes it, and it is
//! evaluated like an update - the studio-internal path, whatever the
//! watch row says. The llm pod's request is the core crate's system
//! prompt, the active tab's current score, and the player's prompt; a
//! failure keeps the editor untouched and lands in the log under `pod`,
//! its message naming the kind of failure.
//!
//! A pod's own words - the command's streams, each line as it completes -
//! go to the log under `pod` through the same bounded capture, and a
//! running pod of any kind is a row in the jobs sheet until it is done.
//!
//! At most one pod runs at a time, whatever the kind, and a prompt
//! submitted while one runs is refused rather than queued: the pod owns
//! the score while it works, and two of them writing one file is a race
//! nobody asked for. Cancellation is not offered for the same reason -
//! killing a pod mid-write leaves a half-written score, which is the
//! worse way to lose one. A command pod that outlives the studio keeps
//! running: nothing kills it on the way out. The agent is the one pod
//! that does not outlive the studio: it dies with it, on the same
//! orderly shutdown the RPC protocol itself uses. A dead or crashed
//! agent is said in the log, and the next submit spawns a fresh one -
//! with a fresh context to build, which is the cost of the crash, not
//! of the design. One agent process exists at a time.
//!
//! The llm key is a settings row or the `RUSTEL_LLM_KEY` environment
//! variable; the environment wins, for players who would rather not
//! keep the key in the preferences file, which stores it as plain text.

use super::*;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::path::Path;
use std::process::{Child, Stdio};
use unicode_width::UnicodeWidthChar;

/// The most lines of one pod's output the studio keeps, and the most
/// bytes. Both keep the tail: a pod that talks all night costs the log
/// panel a bounded slice of itself, never the whole ring.
pub(super) const CAPTURE_MAX_LINES: usize = 200;
pub(super) const CAPTURE_MAX_BYTES: usize = 64 * 1024;

/// The most one line may hold before it is cut. The rest of a longer
/// line is read away rather than kept, so a pod that answers in one
/// enormous line cannot cost the studio the memory of it.
const CAPTURE_LINE_MAX: usize = 4096;

/// How many lines one turn takes from the running pod. The capture
/// cannot retain more than [`CAPTURE_MAX_LINES`] of them anyway.
const LINES_PER_TURN: usize = CAPTURE_MAX_LINES;

/// How many read lines may wait for the turn loop between the readers
/// and the studio. A pod that streams faster than the loop drains fills
/// this, and the reader blocks on it - which backpressures the pod
/// through its own output pipe, the way any full pipe does - instead of
/// growing the studio's memory until the pod exits. The loop drains on
/// every turn while a pod runs, so the queue always empties.
const READ_QUEUE: usize = 256;

/// The most a prompt holds. A prompt is a sentence or two for the pod;
/// a book is a score, and belongs in the file the pod rewrites.
const MAX_REQUEST_CHARS: usize = 2000;

/// The most the command template holds. It is one command line with no
/// quoting, so this is well past anything usable.
const MAX_COMMAND_CHARS: usize = 500;

/// The most an llm endpoint, model or key prompt holds. A key can be a
/// long bearer token rather than a short string, so this is roomier
/// than the command template's bound.
const MAX_CONFIG_CHARS: usize = 2000;

/// How long the agent's own orderly exit is waited for when the studio
/// shuts it down, and how often it is looked for. Closing stdin is the
/// RPC protocol's own way down, so that goes first and the agent gets a
/// moment to finish its writes; what is still up after this is killed,
/// because the studio will not be reading whatever it writes next.
const AGENT_EXIT_WAIT: Duration = Duration::from_secs(2);
const AGENT_EXIT_POLL: Duration = Duration::from_millis(20);

/// What the one pod prompt is asking for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PodPromptKind {
    /// A prompt to hand to the pod now, from the pod prompt's keybind.
    Request,
    /// The command template itself, from Enter on the settings row.
    Command,
    /// The llm endpoint row's base URL, from Enter on its settings row.
    Endpoint,
    /// The llm model row's model name, from Enter on its settings row.
    Model,
    /// The llm key row's API key, from Enter on its settings row.
    Key,
    /// The agent command row's command line, from Enter on its settings
    /// row.
    AgentCommand,
}

impl PodPromptKind {
    /// The prompt box's title.
    fn title(self) -> &'static str {
        match self {
            Self::Request => " pod ",
            Self::Command => " pod command ",
            Self::Endpoint => " llm endpoint ",
            Self::Model => " llm model ",
            Self::Key => " llm key ",
            Self::AgentCommand => " agent command ",
        }
    }

    /// The settings field this prompt edits, as the kept, cleared and
    /// unchanged lines name it. `None` for a request, which keeps
    /// nothing.
    fn setting_name(self) -> Option<&'static str> {
        match self {
            Self::Request => None,
            Self::Command => Some("pod command"),
            Self::Endpoint => Some("llm endpoint"),
            Self::Model => Some("llm model"),
            Self::Key => Some("llm key"),
            Self::AgentCommand => Some("agent command"),
        }
    }

    /// The most characters this prompt holds.
    fn char_limit(self) -> usize {
        match self {
            Self::Request => MAX_REQUEST_CHARS,
            Self::Command | Self::AgentCommand => MAX_COMMAND_CHARS,
            Self::Endpoint | Self::Model | Self::Key => MAX_CONFIG_CHARS,
        }
    }
}

/// The pod prompt: one line of text over the studio, and what it is for.
#[derive(Clone, Debug)]
pub(super) struct PodPrompt {
    kind: PodPromptKind,
    text: String,
    /// The caret's byte offset. Always on a character boundary.
    cursor: usize,
    /// The whole text is one selection: the next insert replaces it.
    selected: bool,
}

impl PodPrompt {
    /// The text the prompt's one row shows, and the caret's column in it:
    /// the tail that holds the caret, the way a terminal line reads.
    /// Both are laid out in display cells rather than characters, so a
    /// wide character still fits the row and the caret column is where
    /// the terminal draws it; a wide character with no room left is left
    /// out rather than half-drawn.
    fn window(&self, width: usize) -> (String, u16) {
        let width = width.max(1);
        let characters: Vec<char> = self.text.chars().collect();
        let caret = self.text[..self.cursor].chars().count();
        // Walk back from the caret, keeping whole characters whose cells
        // still fit beside it.
        let mut first = caret;
        let mut before = 0;
        while first > 0 {
            let cells = characters[first - 1].width().unwrap_or(0);
            if before + cells > width - 1 {
                break;
            }
            before += cells;
            first -= 1;
        }
        // Walk forward with the rest of the room.
        let mut last = caret;
        let mut after = 0;
        while last < characters.len() {
            let cells = characters[last].width().unwrap_or(0);
            if before + after + cells > width {
                break;
            }
            after += cells;
            last += 1;
        }
        let shown: String = characters[first..last].iter().collect();
        (shown, before as u16)
    }

    fn insert(&mut self, text: &str) {
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        let limit = self.kind.char_limit();
        let room = limit.saturating_sub(self.text.chars().count());
        let kept: String = text.chars().take(room).collect();
        if kept.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &kept);
        self.cursor += kept.len();
    }
}

/// The arguments the pod command runs with, from the template the player
/// configured. `None` when the template is empty: the feature is off.
///
/// `{prompt}` is what the prompt held, `{set}` the set folder the pod
/// runs in, and `{file}` the active score's path. Each placeholder stays
/// one argument however many spaces its value holds, and the prompt's
/// own words are never re-expanded: `{set}` and `{file}` are replaced
/// first and `{prompt}` last, so a prompt that quotes a placeholder
/// reaches the pod as the words the player typed. A template with no
/// `{prompt}` gets the prompt appended as its final argument, so a bare
/// program name is a complete configuration.
pub(super) fn pod_arguments(
    template: &str,
    prompt: &str,
    set: &Path,
    file: Option<&Path>,
) -> Option<Vec<String>> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }
    let file = file.map(|file| file.display().to_string());
    let mut arguments: Vec<String> = template
        .split_whitespace()
        .map(|argument| {
            argument
                .replace("{set}", &set.display().to_string())
                .replace("{file}", file.as_deref().unwrap_or_default())
                .replace("{prompt}", prompt)
        })
        .collect();
    if !template.contains("{prompt}") {
        arguments.push(prompt.to_owned());
    }
    Some(arguments)
}

/// Why a request prompt cannot open right now, in the words the footer
/// says. `None`: the configured kind is ready and the slot is the only
/// thing left to ask about.
fn request_refusal(settings: &UiSettings) -> Option<String> {
    match settings.pod_kind {
        PodKind::Command if settings.pod_command.trim().is_empty() => Some(
            "no pod command is set \u{b7} Settings \u{25b8} Live coding \u{25b8} pod command".into(),
        ),
        PodKind::Llm if settings.llm_endpoint.trim().is_empty() => Some(
            "no llm endpoint is set \u{b7} Settings \u{25b8} Live coding \u{25b8} llm endpoint".into(),
        ),
        PodKind::Llm if settings.llm_model.trim().is_empty() => {
            Some("no llm model is set \u{b7} Settings \u{25b8} Live coding \u{25b8} llm model".into())
        }
        PodKind::Llm if settings.llm_key().trim().is_empty() => Some(
            "no llm key is set \u{b7} Settings \u{25b8} Live coding \u{25b8} llm key \u{b7} or the RUSTEL_LLM_KEY environment variable"
                .into(),
        ),
        PodKind::Agent if settings.agent_command.trim().is_empty() => Some(
            "no agent command is set \u{b7} Settings \u{25b8} Live coding \u{25b8} agent command".into(),
        ),
        _ => None,
    }
}

/// The user half of the rewrite request: the score as it stands, and
/// the change the prompt asks for. The system half is the core crate's
/// constant, which asks the model to reply with the complete rewritten
/// score alone.
fn llm_user_message(score: &str, prompt: &str) -> String {
    format!("The current score:\n\n{score}\n\nThe change: {prompt}")
}

/// The bounded tail of one pod's output.
#[derive(Default)]
pub(super) struct Capture {
    lines: VecDeque<String>,
    bytes: usize,
    /// Lines the caps pushed out, oldest first.
    dropped: usize,
}

impl Capture {
    /// Keep one more line, dropping the oldest from the front while
    /// either cap is over.
    fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > CAPTURE_MAX_LINES || self.bytes > CAPTURE_MAX_BYTES {
            match self.lines.pop_front() {
                Some(dropped) => {
                    self.bytes -= dropped.len();
                    self.dropped += 1;
                }
                None => break,
            }
        }
    }

    /// What the completion line says about what the caps did.
    fn truncation_note(&self) -> String {
        if self.dropped > 0 {
            format!(" \u{b7} {} earlier lines dropped", self.dropped)
        } else {
            String::new()
        }
    }
}

/// One pod, running: what the jobs row and the log name it, the work
/// itself, and the bounded tail of what it has said.
pub(super) struct PodRun {
    /// The program that is running, as the jobs row and the log name it:
    /// the command's program, or the model an llm pod asks.
    pub(super) program: String,
    /// What is running: the command's child process, or the llm rewrite
    /// on its own thread.
    body: PodBody,
    capture: Capture,
}

/// What one running pod is.
enum PodBody {
    /// The command-template pod: the child, and the bounded queue its
    /// readers fill.
    Command {
        receiver: std::sync::mpsc::Receiver<String>,
        child: Child,
    },
    /// The llm pod: the rewrite on its own thread. `outcome` carries its
    /// one answer; `scene` is the tab whose score was sent, so the
    /// answer finds the score it was asked about whatever tab is in
    /// front when it lands.
    Llm {
        outcome: std::sync::mpsc::Receiver<LlmOutcome>,
        scene: SceneId,
    },
    /// An agent prompt in flight: the id its `prompt` command went out
    /// under, and the answer's last line still waiting for its newline.
    /// The process, its stdin and its feed live in [`App::pod_agent`],
    /// which outlives the prompt.
    Agent { id: String, pending: String },
}

/// One line from the agent's two pipes: a protocol record off its
/// stdout, or a line of its own diagnostics off its stderr - which the
/// RPC protocol keeps for exactly that, and the log takes the way it
/// takes a command pod's stderr.
enum AgentFeed {
    Record(String),
    Diagnostics(String),
}

/// The agent pod's process: one agent, kept alive across prompts so its
/// context accumulates. A prompt in flight is a [`PodRun`] like any
/// pod's; this is the process underneath, which outlives it.
pub(super) struct AgentPod {
    /// The program as the log and the footer name it: the command line's
    /// first word.
    pub(super) program: String,
    /// The agent's stdin: every RPC command goes here as one JSON object
    /// per line. Taken (closed) on the way down.
    stdin: Option<std::process::ChildStdin>,
    /// The agent's records and diagnostics, from the reader threads.
    feed: std::sync::mpsc::Receiver<AgentFeed>,
    child: Child,
    /// One past the last prompt id handed out. Ids begin at `pod-1` and
    /// correlate each prompt with the response that answers it.
    next_id: u64,
}

impl AgentPod {
    /// The next prompt's command: its id, and the line as it goes out.
    /// One JSON object per line is the protocol's whole framing.
    fn prompt_line(&mut self, message: &str) -> (String, String) {
        let id = format!("pod-{}", self.next_id);
        self.next_id += 1;
        let line = serde_json::json!({
            "id": id,
            "type": "prompt",
            "message": message,
        })
        .to_string();
        (id, line)
    }

    /// Send one command line. A broken pipe here means the agent is
    /// gone; the caller says so, and the next submit spawns a fresh one.
    fn send(&mut self, line: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        };
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// How the process ended, if it already has: for the log line that
    /// says the agent is gone.
    fn how_it_ended(&mut self) -> Option<String> {
        self.child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.to_string())
    }
}

impl Drop for AgentPod {
    /// The agent dies with the studio. Closing stdin is the RPC
    /// protocol's own orderly shutdown, so it goes first and the agent
    /// gets [`AGENT_EXIT_WAIT`] to finish its writes; what is still up
    /// after that is killed, mid-write or not, because the studio will
    /// not be reading whatever it writes next.
    fn drop(&mut self) {
        self.stdin.take();
        let deadline = Instant::now() + AGENT_EXIT_WAIT;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(AGENT_EXIT_POLL),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the llm pod's thread sends back: the rewritten score, or the
/// failure as its message names it - the endpoint's status, the
/// transport fault, the reply that was not the chat JSON it promised.
type LlmOutcome = Result<String, String>;

/// Where the prompt box goes: a one-row sheet under the header, centered.
pub(super) fn prompt_geometry(frame: Rect) -> Rect {
    let width = 64.min(frame.width.saturating_sub(4));
    Rect::new(
        frame.x + frame.width.saturating_sub(width) / 2,
        frame.y + 2,
        width,
        3,
    )
}

/// The prompt, drawn over whatever is open beneath it.
pub(super) fn render_prompt(frame: &mut ratatui::Frame<'_>, theme: &Theme, prompt: &PodPrompt) {
    let area = prompt_geometry(frame.area());
    if area.width < 8 || frame.area().height < 6 {
        return;
    }
    super::super::graphics::cover_images(area);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(prompt.kind.title())
            .style(Style::default().bg(theme.overlay).fg(theme.foreground)),
        area,
    );
    let inner = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);
    let (text, caret) = prompt.window(usize::from(inner.width));
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(theme.foreground)),
        inner,
    );
    frame.set_cursor_position((inner.x + caret.min(inner.width.saturating_sub(1)), inner.y));
}

/// Read one of the pod's streams to its end, sending each line as it
/// completes. A line longer than [`CAPTURE_LINE_MAX`] is sent as its head
/// with an ellipsis; the rest of it is read away rather than kept, so no
/// pod can cost the studio the memory of one enormous answer.
fn forward_stream<R: std::io::Read>(reader: R, sender: std::sync::mpsc::SyncSender<String>) {
    forward_tagged_stream(reader, sender, |line| line)
}

/// The same reader with each line tagged on its way out, so one queue
/// can carry an agent's protocol records and its stderr diagnostics
/// apart.
fn forward_tagged_stream<R, T, F>(mut reader: R, sender: std::sync::mpsc::SyncSender<T>, tag: F)
where
    R: std::io::Read,
    F: Fn(String) -> T,
{
    let mut pending: Vec<u8> = Vec::new();
    // The tail of a line whose head was already sent: read away, unsent.
    let mut discarding = false;
    let mut chunk = [0u8; 1024];
    loop {
        // A line at a time out of what is already in hand.
        while let Some(at) = pending.iter().position(|byte| *byte == b'\n') {
            if discarding {
                pending.drain(..=at);
                discarding = false;
                continue;
            }
            if at > CAPTURE_LINE_MAX {
                let head: Vec<u8> = pending.drain(..CAPTURE_LINE_MAX).collect();
                if send_line(&sender, head, true, &tag).is_err() {
                    return;
                }
                pending.drain(..=at - CAPTURE_LINE_MAX);
                continue;
            }
            let line: Vec<u8> = pending.drain(..=at).collect();
            if send_line(&sender, line, false, &tag).is_err() {
                // The studio is gone; the pipe is closed behind it anyway.
                return;
            }
        }
        // Still no newline: an overlong line is already underway, so its
        // head goes out now and whatever follows is thrown away until
        // its own newline ends it. A line of exactly the bound still
        // has room for its own newline in the next chunk, so only a
        // longer one is cut.
        if !discarding && pending.len() > CAPTURE_LINE_MAX {
            let head: Vec<u8> = pending.drain(..CAPTURE_LINE_MAX).collect();
            if send_line(&sender, head, true, &tag).is_err() {
                return;
            }
            discarding = true;
        }
        // While discarding, nothing below the newline is kept: the bytes
        // past the head never had anywhere to go.
        if discarding {
            pending.clear();
        }
        match reader.read(&mut chunk) {
            // The end of the stream: a last line with no newline of its
            // own is still a line.
            Ok(0) | Err(_) => {
                if !discarding && !pending.is_empty() {
                    let _ = send_line(&sender, pending, false, &tag);
                }
                return;
            }
            Ok(count) => pending.extend_from_slice(&chunk[..count]),
        }
    }
}

/// One line as one event, without its newline, tagged for its queue and
/// marked when it was cut.
fn send_line<T, F>(
    sender: &std::sync::mpsc::SyncSender<T>,
    mut bytes: Vec<u8>,
    cut: bool,
    tag: &F,
) -> Result<(), std::sync::mpsc::SendError<()>>
where
    F: Fn(String) -> T,
{
    while matches!(bytes.last(), Some(b'\n') | Some(b'\r')) {
        bytes.pop();
    }
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        line.push_str(" \u{2026}");
    }
    sender
        .send(tag(line))
        .map_err(|_| std::sync::mpsc::SendError(()))
}

impl App {
    /// Open the pod prompt. A request needs the configured kind's rows
    /// filled in and a free pod slot; a settings row's own edit prompt
    /// always opens, over the sheet it came from.
    pub(super) fn open_pod_prompt(&mut self, kind: PodPromptKind) {
        if kind == PodPromptKind::Request {
            if let Some(refusal) = request_refusal(&self.ui_settings) {
                self.status = refusal;
                self.dirty_frame = true;
                return;
            }
            if self.pod_run.is_some() {
                self.status = "a pod is already running \u{b7} its output is in the log".into();
                self.dirty_frame = true;
                return;
            }
            self.dismiss_dialogs(None);
        }
        let text = match kind {
            PodPromptKind::Command => self.ui_settings.pod_command.clone(),
            PodPromptKind::AgentCommand => self.ui_settings.agent_command.clone(),
            PodPromptKind::Endpoint => self.ui_settings.llm_endpoint.clone(),
            PodPromptKind::Model => self.ui_settings.llm_model.clone(),
            PodPromptKind::Key => self.ui_settings.llm_api_key.clone(),
            PodPromptKind::Request => String::new(),
        };
        let cursor = text.len();
        self.pod_prompt = Some(PodPrompt {
            kind,
            text,
            cursor,
            selected: kind != PodPromptKind::Request,
        });
        self.status = match kind {
            PodPromptKind::Request => match self.ui_settings.pod_kind {
                PodKind::Command => format!(
                    "pod - type a prompt \u{b7} Enter runs {} \u{b7} Esc closes",
                    self.ui_settings
                        .pod_command
                        .split_whitespace()
                        .next()
                        .unwrap_or("the pod")
                ),
                PodKind::Llm => format!(
                    "pod - type a prompt \u{b7} Enter asks {} \u{b7} Esc closes",
                    self.ui_settings.llm_model.trim()
                ),
                PodKind::Agent => format!(
                    "pod - type a prompt \u{b7} Enter asks {} \u{b7} Esc closes",
                    self.ui_settings
                        .agent_command
                        .split_whitespace()
                        .next()
                        .unwrap_or("the agent")
                ),
            },
            PodPromptKind::Command => {
                "pod command - {prompt} {set} {file} are replaced \u{b7} Enter keeps \u{b7} Esc closes"
                    .into()
            }
            PodPromptKind::Endpoint => {
                "llm endpoint - the base URL ahead of /chat/completions \u{b7} Enter keeps \u{b7} Esc closes"
                    .into()
            }
            PodPromptKind::Model => {
                "llm model - the name the request sends \u{b7} Enter keeps \u{b7} Esc closes".into()
            }
            PodPromptKind::Key => {
                "llm key - Enter keeps \u{b7} RUSTEL_LLM_KEY in the environment wins over it \u{b7} Esc closes"
                    .into()
            }
            PodPromptKind::AgentCommand => {
                "agent command - the RPC process one prompt after another goes to \u{b7} Enter keeps \u{b7} Esc closes"
                    .into()
            }
        };
        self.dirty_frame = true;
    }

    /// Keys and pastes while the pod prompt has the keyboard. `true`
    /// when it took them. Anything else - the mouse, a resize, focus
    /// loss - passes through to whatever is beneath, exactly as the
    /// find bar and the quick action pass them on.
    ///
    /// The transport stays global, the way it does from the find bar:
    /// stopping the music works from a prompt. Quit is answered before
    /// this is offered.
    pub(super) fn pod_prompt_event(&mut self, terminal_event: &Event) -> bool {
        if self.pod_prompt.is_none() {
            return false;
        }
        // Only keys and pastes are the prompt's; everything else - the
        // mouse, a resize, focus loss - passes through to whatever is
        // beneath, exactly as the find bar and the quick action pass
        // them on. A key release reaches the body below, which keeps it
        // from falling through to the surfaces under the prompt.
        if !matches!(terminal_event, Event::Key(_)) && !matches!(terminal_event, Event::Paste(_)) {
            return false;
        }
        if let Event::Key(key) = terminal_event
            && key.kind == KeyEventKind::Press
            && matches!(
                self.keybinds.action_for(key),
                Some(BindAction::Evaluate)
                    | Some(BindAction::Stop)
                    | Some(BindAction::RewindEvaluate)
            )
        {
            return false;
        }
        let Some(mut prompt) = self.pod_prompt.take() else {
            return false;
        };
        match terminal_event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let primary = key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                match key.code {
                    KeyCode::Esc if key.kind == KeyEventKind::Press => {
                        self.status = match prompt.kind.setting_name() {
                            Some(name) => format!("{name} unchanged"),
                            None => "pod prompt closed".into(),
                        };
                        self.dirty_frame = true;
                        return true;
                    }
                    KeyCode::Enter
                        if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                    {
                        self.submit_pod_prompt(prompt);
                        return true;
                    }
                    KeyCode::Char('a' | 'A') if primary && !alt => prompt.selected = true,
                    KeyCode::Char('v' | 'V')
                        if primary && !alt && key.kind == KeyEventKind::Press =>
                    {
                        match self.clipboard.get_text() {
                            Ok(text) => {
                                prompt.insert(&super::super::file_picker::single_line_paste(&text))
                            }
                            Err(error) => self.status = format!("could not paste: {error}"),
                        }
                    }
                    KeyCode::Char(character) if !primary && !alt => {
                        prompt.insert(&character.to_string());
                    }
                    KeyCode::Backspace if !primary && !alt => {
                        if prompt.selected {
                            prompt.text.clear();
                            prompt.cursor = 0;
                            prompt.selected = false;
                        } else if prompt.cursor > 0 {
                            let mut at = prompt.cursor - 1;
                            while !prompt.text.is_char_boundary(at) {
                                at -= 1;
                            }
                            prompt.cursor = at;
                            prompt.text.remove(at);
                        }
                    }
                    KeyCode::Delete if !primary && !alt => {
                        if prompt.selected {
                            prompt.text.clear();
                            prompt.cursor = 0;
                            prompt.selected = false;
                        } else if prompt.cursor < prompt.text.len() {
                            prompt.text.remove(prompt.cursor);
                        }
                    }
                    KeyCode::Left if !primary && !alt => {
                        prompt.cursor = if prompt.selected {
                            0
                        } else {
                            let mut at = prompt.cursor.saturating_sub(1);
                            while at > 0 && !prompt.text.is_char_boundary(at) {
                                at -= 1;
                            }
                            at
                        };
                        prompt.selected = false;
                    }
                    KeyCode::Right if !primary && !alt => {
                        if prompt.selected {
                            prompt.cursor = 0;
                            prompt.selected = false;
                        } else {
                            prompt.cursor = (prompt.cursor + 1).min(prompt.text.len());
                            while prompt.cursor < prompt.text.len()
                                && !prompt.text.is_char_boundary(prompt.cursor)
                            {
                                prompt.cursor += 1;
                            }
                        }
                    }
                    KeyCode::Home if !primary && !alt => {
                        prompt.cursor = 0;
                        prompt.selected = false;
                    }
                    KeyCode::End if !primary && !alt => {
                        prompt.cursor = prompt.text.len();
                        prompt.selected = false;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                prompt.insert(&super::super::file_picker::single_line_paste(text));
            }
            _ => {}
        }
        self.pod_prompt = Some(prompt);
        self.dirty_frame = true;
        true
    }

    /// Enter on the prompt: keep the settings field it edits, or start
    /// the pod.
    fn submit_pod_prompt(&mut self, prompt: PodPrompt) {
        match prompt.kind {
            PodPromptKind::Request => {
                let text = prompt.text.trim().to_owned();
                if text.is_empty() {
                    // The prompt stays open: an empty submit is a slip of
                    // the key, not a request for nothing.
                    self.pod_prompt = Some(prompt);
                    self.status = "type what to ask the pod \u{b7} Esc closes".into();
                    self.dirty_frame = true;
                    return;
                }
                self.launch_request(&text);
            }
            _ => self.keep_pod_setting(prompt),
        }
    }

    /// Start the kind of pod the settings name, with the prompt.
    fn launch_request(&mut self, prompt: &str) {
        match self.ui_settings.pod_kind {
            // The command-template pod, untouched by the llm rows.
            PodKind::Command => self.launch_pod(prompt),
            PodKind::Llm => self.launch_llm_pod(prompt),
            // The persistent agent: one process, the prompt one RPC
            // command in its life.
            PodKind::Agent => self.launch_agent_pod(prompt),
        }
    }

    /// Keep the settings field the prompt edits, the way the settings
    /// sheet keeps any other change.
    fn keep_pod_setting(&mut self, prompt: PodPrompt) {
        let name = prompt
            .kind
            .setting_name()
            .expect("a settings edit, not a request");
        let text = prompt.text.trim().to_owned();
        match prompt.kind {
            PodPromptKind::Command => self.ui_settings.pod_command = text.clone(),
            PodPromptKind::AgentCommand => self.ui_settings.agent_command = text.clone(),
            PodPromptKind::Endpoint => self.ui_settings.llm_endpoint = text.clone(),
            PodPromptKind::Model => self.ui_settings.llm_model = text.clone(),
            PodPromptKind::Key => self.ui_settings.llm_api_key = text.clone(),
            PodPromptKind::Request => unreachable!("a request keeps nothing"),
        }
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        // The key is never echoed back; everything else is, as the
        // sheet's own row would show it.
        self.status = if prompt.kind == PodPromptKind::Key {
            if text.is_empty() {
                "llm key cleared".into()
            } else {
                "llm key kept \u{b7} the preferences file keeps it as plain text".into()
            }
        } else if text.is_empty() {
            match prompt.kind {
                PodPromptKind::Command => {
                    "pod command cleared \u{b7} the pod prompt is hidden until one is set".into()
                }
                PodPromptKind::AgentCommand => {
                    "agent command cleared \u{b7} agent pods are refused until one is set".into()
                }
                _ => format!("{name} cleared"),
            }
        } else {
            format!("{name} kept \u{b7} {text}")
        };
        self.dirty_frame = true;
    }

    /// Spawn the configured pod with the prompt, in the set folder.
    fn launch_pod(&mut self, prompt: &str) {
        let template = self.ui_settings.pod_command.trim().to_owned();
        if template.is_empty() {
            self.status = "no pod command is set \u{b7} Settings \u{25b8} Live coding".into();
            self.dirty_frame = true;
            return;
        }
        if self.pod_run.is_some() {
            self.status = "a pod is already running \u{b7} its output is in the log".into();
            self.dirty_frame = true;
            return;
        }
        // The set and the score keep the spelling they were opened with,
        // which can be relative to the studio's own cwd. The pod runs with
        // the set folder as its cwd, so that spelling would resolve
        // against the wrong directory. Hand it the same paths, absolute.
        let set = std::path::absolute(self.scenes.directory())
            .unwrap_or_else(|_| self.scenes.directory().to_path_buf());
        let file = self.scenes.current().is_score().then(|| {
            std::path::absolute(&self.scenes.current().path)
                .unwrap_or_else(|_| self.scenes.current().path.clone())
        });
        if template.contains("{file}") && file.is_none() {
            self.status = "the pod command names {file} but no score is open".into();
            self.dirty_frame = true;
            return;
        }
        let arguments = pod_arguments(&template, prompt, &set, file.as_deref())
            .expect("the template is not empty");
        let program = arguments.first().cloned().unwrap_or_default();
        let spawn = std::process::Command::new(&program)
            .args(&arguments[1..])
            .current_dir(&set)
            // The pod runs unattended: it cannot ask a question, and it
            // is configured for that - non-interactive flags and all.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match spawn {
            Ok(mut child) => {
                let (sender, receiver) = std::sync::mpsc::sync_channel(READ_QUEUE);
                if let Some(stdout) = child.stdout.take() {
                    let sender = sender.clone();
                    std::thread::spawn(move || {
                        forward_stream(std::io::BufReader::new(stdout), sender)
                    });
                }
                if let Some(stderr) = child.stderr.take() {
                    std::thread::spawn(move || {
                        forward_stream(std::io::BufReader::new(stderr), sender)
                    });
                }
                self.pod_run = Some(PodRun {
                    program: program.clone(),
                    body: PodBody::Command { receiver, child },
                    capture: Capture::default(),
                });
                self.log
                    .push(LogLevel::Info, "pod", format!("started {program}"));
                self.status = format!("pod running \u{b7} {program}");
                self.dirty_frame = true;
            }
            Err(error) => {
                self.log.push(
                    LogLevel::Warn,
                    "pod",
                    format!("could not start {program}: {error}"),
                );
                self.status = format!("could not start the pod: {error}");
                self.dirty_frame = true;
            }
        }
    }

    /// Ask the configured llm endpoint for a rewrite of the active tab's
    /// score, on the pod's background thread. The answer comes back
    /// through [`App::pump_pod`], the way a command pod's lines do.
    fn launch_llm_pod(&mut self, prompt: &str) {
        if self.pod_run.is_some() {
            self.status = "a pod is already running \u{b7} its output is in the log".into();
            self.dirty_frame = true;
            return;
        }
        // The rows were checked when the prompt opened; the settings can
        // have moved under an open prompt, so the launch checks them too,
        // the way the command pod re-checks its template.
        let endpoint = self.ui_settings.llm_endpoint.trim().to_owned();
        let model = self.ui_settings.llm_model.trim().to_owned();
        let key = self.ui_settings.llm_key().trim().to_owned();
        if endpoint.is_empty() || model.is_empty() || key.is_empty() {
            self.status =
                "the llm pod needs an endpoint, a model and a key \u{b7} Settings \u{25b8} Live coding"
                    .into();
            self.dirty_frame = true;
            return;
        }
        let scene = self.scenes.current();
        if !scene.is_score() {
            self.status = "no score is open \u{b7} the llm pod rewrites the open score".into();
            self.dirty_frame = true;
            return;
        }
        let scene_id = scene.id;
        let score = scene.editor.source();
        let user = llm_user_message(&score, prompt);
        let endpoint = rustel_pod_llm::Endpoint::new(endpoint, model.clone(), key);
        let (sender, outcome) = std::sync::mpsc::channel::<LlmOutcome>();
        let spawned = std::thread::Builder::new()
            .name("pod-llm".into())
            .spawn(move || {
                // One call on this thread alone: the studio's turn loop
                // never waits on the network. The failure keeps its
                // message, which names the kind of failure it was.
                let answer = endpoint
                    .rewrite(rustel_pod_llm::STRUDEL_SYSTEM_PROMPT, &user)
                    .map_err(|error| error.to_string());
                let _ = sender.send(answer);
            });
        match spawned {
            Ok(_) => {
                self.pod_run = Some(PodRun {
                    program: model.clone(),
                    body: PodBody::Llm {
                        outcome,
                        scene: scene_id,
                    },
                    capture: Capture::default(),
                });
                self.log
                    .push(LogLevel::Info, "pod", format!("asking {model}"));
                self.status = format!("pod running \u{b7} asking {model}");
                self.dirty_frame = true;
            }
            Err(error) => {
                self.log.push(
                    LogLevel::Warn,
                    "pod",
                    format!("could not start the llm pod: {error}"),
                );
                self.status = format!("could not start the pod: {error}");
                self.dirty_frame = true;
            }
        }
    }

    /// Start the agent pod: one process, spawned if none is alive, and
    /// the prompt sent to it as one RPC `prompt` command under an id of
    /// its own. The agent's answer streams into the log through
    /// [`App::pump_pod`], the way a command pod's lines do; the prompt
    /// is done when the agent settles, and the process stays alive for
    /// the next one.
    fn launch_agent_pod(&mut self, prompt: &str) {
        if self.pod_run.is_some() {
            self.status = "a pod is already running \u{b7} its output is in the log".into();
            self.dirty_frame = true;
            return;
        }
        let template = self.ui_settings.agent_command.trim().to_owned();
        if template.is_empty() {
            self.status =
                "no agent command is set \u{b7} Settings \u{25b8} Live coding \u{25b8} agent command".into();
            self.dirty_frame = true;
            return;
        }
        // One agent process at a time: a living one takes the prompt as
        // it stands, and a dead one was already taken away and said by
        // the turn loop, so what is left here is the first prompt of the
        // night.
        if self.pod_agent.is_none()
            && let Err(error) = self.spawn_agent(&template)
        {
            let program = template.split_whitespace().next().unwrap_or("the agent");
            self.log.push(
                LogLevel::Warn,
                "pod",
                format!("could not start {program}: {error}"),
            );
            self.status = format!("could not start the agent: {error}");
            self.dirty_frame = true;
            return;
        }
        let Some(agent) = self.pod_agent.as_mut() else {
            return;
        };
        let (id, line) = agent.prompt_line(prompt);
        let sent = agent.send(&line);
        let program = agent.program.clone();
        match sent {
            Ok(()) => {
                self.pod_run = Some(PodRun {
                    program: program.clone(),
                    body: PodBody::Agent {
                        id,
                        pending: String::new(),
                    },
                    capture: Capture::default(),
                });
                self.log.push(
                    LogLevel::Info,
                    "pod",
                    format!("sent the prompt to {program}"),
                );
                self.status = format!("pod running \u{b7} {program} is on it");
                self.dirty_frame = true;
            }
            Err(error) => {
                // The pipe was already broken: the agent went between the
                // turn loop's last look and this submit. Take it away and
                // say so - the next submit spawns a fresh one.
                let how = self.agent_gone();
                self.log.push(
                    LogLevel::Warn,
                    "pod",
                    format!("the prompt never reached {program}: the agent {how} ({error})"),
                );
                self.status = format!("the agent {how} \u{b7} submit the prompt again");
                self.dirty_frame = true;
            }
        }
    }

    /// Spawn the agent: one process that stays alive across prompts,
    /// speaking the RPC protocol - one JSON object per line, commands
    /// on stdin, responses and events on stdout - with the set folder as
    /// its working directory, so its own file tools land in the set.
    /// No shell and no placeholders: the template is the command line,
    /// and the prompt travels in the protocol, not in the arguments.
    fn spawn_agent(&mut self, template: &str) -> Result<(), String> {
        let arguments: Vec<String> = template.split_whitespace().map(str::to_owned).collect();
        let program = arguments.first().cloned().unwrap_or_default();
        // The set keeps the spelling it was opened with, which can be
        // relative to the studio's own cwd; the agent's working
        // directory needs the absolute one, the way the command pod's
        // does.
        let set = std::path::absolute(self.scenes.directory())
            .unwrap_or_else(|_| self.scenes.directory().to_path_buf());
        let spawn = std::process::Command::new(&program)
            .args(&arguments[1..])
            .current_dir(&set)
            // The agent is the one pod the studio talks to: its stdin is
            // the protocol, not a closed door.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match spawn {
            Ok(mut child) => {
                let stdin = child.stdin.take();
                let (sender, feed) = std::sync::mpsc::sync_channel(READ_QUEUE);
                if let Some(stdout) = child.stdout.take() {
                    let sender = sender.clone();
                    std::thread::spawn(move || {
                        forward_tagged_stream(
                            std::io::BufReader::new(stdout),
                            sender,
                            AgentFeed::Record,
                        )
                    });
                }
                if let Some(stderr) = child.stderr.take() {
                    std::thread::spawn(move || {
                        forward_tagged_stream(
                            std::io::BufReader::new(stderr),
                            sender,
                            AgentFeed::Diagnostics,
                        )
                    });
                }
                self.log.push(
                    LogLevel::Info,
                    "pod",
                    format!("started {program} as the agent"),
                );
                self.pod_agent = Some(AgentPod {
                    program,
                    stdin,
                    feed,
                    child,
                    next_id: 1,
                });
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Take what the running pod has said since the last turn into the
    /// log, and finish a pod whose work is done: a command once its
    /// process has exited and its readers have read the last of it, so
    /// no line is dropped on the way out, an llm rewrite once its thread
    /// has answered, an agent prompt once the agent has settled - while
    /// the agent itself stays alive for the next prompt.
    pub(super) fn pump_pod(&mut self) {
        let run = self.pod_run.take();
        // A living agent gets its turn whatever else is running: its feed
        // must keep moving - a full queue blocks the readers, and through
        // them the agent itself - and a dead one is said in the log, so
        // the next submit spawns fresh. An agent prompt's own pump reads
        // the same feed, so the duty is its alone.
        if !run
            .as_ref()
            .is_some_and(|run| matches!(run.body, PodBody::Agent { .. }))
        {
            self.pump_agent();
        }
        let Some(run) = run else {
            return;
        };
        let PodRun {
            program,
            body,
            mut capture,
        } = run;
        match body {
            PodBody::Command {
                receiver,
                mut child,
            } => {
                let mut drained = false;
                for _ in 0..LINES_PER_TURN {
                    match receiver.try_recv() {
                        Ok(line) => {
                            capture.push(line.clone());
                            self.log.push(LogLevel::Info, "pod", line);
                            self.dirty_frame = true;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            drained = true;
                            break;
                        }
                    }
                }
                match child.try_wait() {
                    // Exited, and everything it said has been read: report it.
                    Ok(Some(status)) if drained => {
                        let outcome = if status.success() {
                            "finished".to_owned()
                        } else {
                            format!("failed ({status})")
                        };
                        self.log.push(
                            LogLevel::Info,
                            "pod",
                            format!("{program} {outcome}{}", capture.truncation_note()),
                        );
                        self.status = format!("pod {outcome}");
                        self.dirty_frame = true;
                    }
                    // Still running, or exited with output still on its way.
                    Ok(_) => {
                        self.pod_run = Some(PodRun {
                            program,
                            body: PodBody::Command { receiver, child },
                            capture,
                        });
                    }
                    Err(error) => {
                        self.log.push(
                            LogLevel::Warn,
                            "pod",
                            format!("{program}: could not ask how it ended: {error}"),
                        );
                        self.dirty_frame = true;
                    }
                }
            }
            PodBody::Llm { outcome, scene } => match outcome.try_recv() {
                Ok(Ok(score)) => self.llm_score_arrived(program, capture, scene, score),
                Ok(Err(message)) => {
                    // The rewrite failed: the editor is untouched, and
                    // the failure is said in the log with the kind of
                    // failure its message names.
                    capture.push(message.clone());
                    self.log.push(
                        LogLevel::Warn,
                        "pod",
                        format!(
                            "{program} failed \u{b7} {message}{}",
                            capture.truncation_note()
                        ),
                    );
                    self.status = format!("pod failed \u{b7} {message}");
                    self.dirty_frame = true;
                }
                Err(TryRecvError::Empty) => {
                    self.pod_run = Some(PodRun {
                        program,
                        body: PodBody::Llm { outcome, scene },
                        capture,
                    });
                }
                Err(TryRecvError::Disconnected) => {
                    self.log.push(
                        LogLevel::Warn,
                        "pod",
                        format!("{program} ended without an answer"),
                    );
                    self.status = "pod ended without an answer".into();
                    self.dirty_frame = true;
                }
            },
            PodBody::Agent { id, pending } => {
                self.pump_agent_prompt(program, id, pending, capture);
            }
        }
    }

    /// One turn of an agent prompt in flight: the agent's records are
    /// parsed out of its feed, its text deltas and tool narration
    /// stream into the capture, and the prompt ends when the agent
    /// settles - while the process stays alive for the next prompt.
    /// See [`agent_record`] for what each record is worth saying.
    fn pump_agent_prompt(
        &mut self,
        program: String,
        id: String,
        mut pending: String,
        mut capture: Capture,
    ) {
        let (items, gone) = self.take_agent_feed();
        let mut settled = None;
        let mut refused = None;
        let mut handled = false;
        for item in items {
            let say = match item {
                // The agent's own diagnostics, kept off its stdout by the
                // protocol: the log takes them the way it takes a command
                // pod's stderr.
                AgentFeed::Diagnostics(line) => AgentSay::raw(line),
                AgentFeed::Record(line) => {
                    match serde_json::from_str::<serde_json::Value>(&line) {
                        Ok(record) => agent_record(&line, &record, &id, &mut pending),
                        // Not the protocol: the agent's own words, like any
                        // pod's output.
                        Err(_) => AgentSay::raw(line),
                    }
                }
            };
            for said in say.lines {
                capture.push(said.clone());
                self.log.push(LogLevel::Info, "pod", said);
                self.dirty_frame = true;
            }
            settled = settled.or(say.settled);
            refused = refused.or(say.refused);
            handled |= say.handled;
        }
        // The prompt is over when the agent settles, refuses it, handles
        // it with no run, or is gone; the tail of the answer, which no
        // newline of its own ended, goes out before the completion line.
        if (settled.is_some() || refused.is_some() || handled || gone) && !pending.is_empty() {
            capture.push(pending.clone());
            let tail = std::mem::take(&mut pending);
            self.log.push(LogLevel::Info, "pod", tail);
            self.dirty_frame = true;
        }
        let note = capture.truncation_note();
        // A prompt that ends on the agent's answer, refusal or handled
        // receipt is reported as such even when the agent exits on the
        // heels of it - any JSONL-RPC program may exit after completing
        // a run, and only a bare exit falls back to the gone report. In
        // every ended case a dead agent is taken away first, so the next
        // submit spawns a fresh one and the log says how it exited.
        let exited = if gone { Some(self.agent_gone()) } else { None };
        if let Some(message) = refused {
            self.log.push(
                LogLevel::Warn,
                "pod",
                format!("{program} refused the prompt \u{b7} {message}{note}"),
            );
            self.status = format!("pod failed \u{b7} {message}");
            self.dirty_frame = true;
            return;
        }
        if handled {
            // An extension command took the prompt with no run of its
            // own: no agent_settled follows, so the prompt ends here.
            self.log.push(
                LogLevel::Info,
                "pod",
                format!("{program} handled the prompt without a run{note}"),
            );
            self.status = "pod finished".into();
            self.dirty_frame = true;
            return;
        }
        if let Some(aborted) = settled {
            if aborted {
                self.log.push(
                    LogLevel::Warn,
                    "pod",
                    format!("{program} gave up \u{b7} the run was aborted{note}"),
                );
                self.status = "pod failed \u{b7} the agent gave up".into();
            } else {
                self.log
                    .push(LogLevel::Info, "pod", format!("{program} answered{note}"));
                self.status = "pod finished \u{b7} the agent answered".into();
            }
            self.dirty_frame = true;
            return;
        }
        if let Some(how) = exited {
            self.log.push(
                LogLevel::Warn,
                "pod",
                format!("{program} failed \u{b7} the agent {how}{note}"),
            );
            self.status = format!("pod failed \u{b7} the agent {how}");
            self.dirty_frame = true;
            return;
        }
        // Still working: the prompt waits for another turn.
        self.pod_run = Some(PodRun {
            program,
            body: PodBody::Agent { id, pending },
            capture,
        });
    }

    /// The agent process's own turn, whenever no agent prompt is reading
    /// its feed - between prompts, or while another kind of pod runs.
    /// Its diagnostics still land in the log; protocol records pass by
    /// unmentioned, because between prompts they say nothing the player
    /// asked for; and a dead one is taken away and said, so the next
    /// submit spawns a fresh one.
    fn pump_agent(&mut self) {
        if self.pod_agent.is_none() {
            return;
        }
        let (items, gone) = self.take_agent_feed();
        for item in items {
            let line = match item {
                AgentFeed::Diagnostics(line) => line,
                AgentFeed::Record(line) => {
                    match serde_json::from_str::<serde_json::Value>(&line) {
                        // The protocol talking to itself: not the
                        // player's business between prompts.
                        Ok(record) if record.get("type").is_some() => continue,
                        // Anything else is the agent's own words.
                        _ => line,
                    }
                }
            };
            self.log.push(LogLevel::Info, "pod", line);
            self.dirty_frame = true;
        }
        if gone {
            let how = self.agent_gone();
            self.status = format!("the agent {how}");
            self.dirty_frame = true;
        }
    }

    /// Up to one turn's worth of the agent's feed, and whether both of
    /// its readers have finished: their end is the protocol's own way of
    /// saying the process is gone.
    fn take_agent_feed(&mut self) -> (Vec<AgentFeed>, bool) {
        let mut items = Vec::new();
        let Some(agent) = self.pod_agent.as_mut() else {
            // No process under a prompt still waiting on one: the prompt
            // ends rather than hangs on a feed nobody fills.
            return (items, true);
        };
        for _ in 0..LINES_PER_TURN {
            match agent.feed.try_recv() {
                Ok(item) => items.push(item),
                Err(TryRecvError::Empty) => return (items, false),
                Err(TryRecvError::Disconnected) => return (items, true),
            }
        }
        (items, false)
    }

    /// Take the agent away - its feed's end means the process is gone,
    /// or will not be spoken to again - and say it in the log, so the
    /// next submit spawns a fresh one. Returns the short form for the
    /// footer.
    fn agent_gone(&mut self) -> String {
        let Some(mut agent) = self.pod_agent.take() else {
            return "is gone".into();
        };
        let how = match agent.how_it_ended() {
            Some(status) => format!("exited: {status}"),
            None => "closed its output".into(),
        };
        self.log.push(
            LogLevel::Warn,
            "pod",
            format!(
                "the agent {} {how} \u{b7} the next prompt starts a fresh one",
                agent.program
            ),
        );
        self.dirty_frame = true;
        how
    }

    /// The rewrite came back: the returned score goes into the tab the
    /// prompt was asked about, its file takes it the way an update's
    /// write does, and it runs like an update - the studio-internal path,
    /// whatever the external-changes row says, because this pane is the
    /// studio's own.
    fn llm_score_arrived(
        &mut self,
        program: String,
        mut capture: Capture,
        scene_id: SceneId,
        score: String,
    ) {
        // The tab can have closed, or the set switched, while the rewrite
        // ran. Nothing to write into: the answer is said and dropped.
        let Some(index) = self.scenes.index_of(scene_id) else {
            self.log.push(
                LogLevel::Warn,
                "pod",
                format!("{program} answered, but the score it rewrote was closed while it ran"),
            );
            self.status = "the rewritten score's tab was closed while the pod ran".into();
            self.dirty_frame = true;
            return;
        };
        // Bring the rewritten tab forward, the way the sync & eval watch
        // brings a saved tab forward: the answer is about that score.
        if self.scenes.current().id != scene_id {
            self.select_scene(index);
        }
        let name = self.scenes.current().name().to_owned();
        let Some(scene) = self.scenes.get_mut(scene_id) else {
            return;
        };
        // One programmatic edit, the way a watched file's text is taken:
        // the caret and the undo history stay put.
        scene.adopt_source(&score);
        self.write_scene(scene_id, Arc::from(score));
        self.note_external_change(scene_id, Instant::now());
        let keep = self.set_panel_selection();
        self.refresh_set_panel(keep);
        capture.push(format!("{program} answered"));
        self.log.push(
            LogLevel::Info,
            "pod",
            format!("{program} rewrote {name}{}", capture.truncation_note()),
        );
        self.status = format!("pod rewrote {name}");
        self.dirty_frame = true;
        // The score plays the way a watched save plays: the decision to
        // run it was the prompt, and this path asks nothing of the
        // external-changes row.
        self.apply_external_change();
    }
}

/// What one record from the agent said, as [`agent_record`] reads it.
#[derive(Default)]
struct AgentSay {
    /// Lines for the capture and the log, in the order they were said.
    lines: Vec<String>,
    /// The agent settled: `Some(true)` with the run aborted, `Some(false)`
    /// with the answer given. `None`: the run is still on.
    settled: Option<bool>,
    /// The response record refused the prompt outright; the message is
    /// why.
    refused: Option<String>,
    /// The response record handled the prompt with no run of its own,
    /// so no `agent_settled` will follow.
    handled: bool,
}

impl AgentSay {
    /// A line that is not the protocol at all: the agent's own words.
    fn raw(line: String) -> Self {
        Self {
            lines: vec![line],
            ..Self::default()
        }
    }
}

/// What one protocol record from the agent says: the lines worth
/// putting in the capture, what the answer added to `pending`, and how
/// the prompt fared. The records the studio does not narrate - the
/// turns starting and ending, the messages' bookkeeping - pass by
/// unmentioned; the ones that say something to the player are the
/// answer's text deltas, the tools running, and the run settling.
fn agent_record(
    line: &str,
    record: &serde_json::Value,
    prompt_id: &str,
    pending: &mut String,
) -> AgentSay {
    let mut say = AgentSay::default();
    match record.get("type").and_then(|kind| kind.as_str()) {
        // The prompt's own receipt. Only the response under this
        // prompt's id counts; anything else is not ours, and is said as
        // it came the way a wrong answer would be.
        Some("response") => {
            if record.get("id").and_then(|id| id.as_str()) != Some(prompt_id) {
                say.lines.push(line.to_owned());
                return say;
            }
            if !record
                .get("success")
                .and_then(|success| success.as_bool())
                .unwrap_or(false)
            {
                say.refused = Some(
                    record
                        .get("error")
                        .and_then(|error| error.as_str())
                        .unwrap_or("without a reason")
                        .to_owned(),
                );
                return say;
            }
            // A prompt an extension handled with no run: no
            // agent_settled follows, so the prompt ends here.
            // `started` and `queued` leave it waiting for one.
            if record
                .pointer("/data/disposition")
                .and_then(|how| how.as_str())
                == Some("handled")
            {
                say.handled = true;
            }
        }
        // The answer, streaming: each delta adds to the pending line,
        // and every newline in it ends one.
        Some("message_update") => {
            let event = &record["assistantMessageEvent"];
            if event.get("type").and_then(|kind| kind.as_str()) == Some("text_delta")
                && let Some(delta) = event.get("delta").and_then(|delta| delta.as_str())
            {
                pending.push_str(delta);
                while let Some(at) = pending.find('\n') {
                    let complete: String = pending.drain(..=at).collect();
                    say.lines
                        .push(complete.trim_end_matches(['\n', '\r']).to_owned());
                }
            }
        }
        // The tools, narrated: what ran, and how it went.
        Some("tool_execution_start") => {
            if let Some(name) = record.get("toolName").and_then(|name| name.as_str()) {
                say.lines.push(format!("tool {name}"));
            }
        }
        Some("tool_execution_end") => {
            if let Some(name) = record.get("toolName").and_then(|name| name.as_str()) {
                let failed = record
                    .get("isError")
                    .and_then(|bad| bad.as_bool())
                    .unwrap_or(false);
                let milliseconds = record.get("durationMs").and_then(|ms| ms.as_u64());
                say.lines.push(match (failed, milliseconds) {
                    (true, _) => format!("tool {name} failed"),
                    (false, Some(ms)) => format!("tool {name} done in {ms} ms"),
                    (false, None) => format!("tool {name} done"),
                });
            }
        }
        // The run is over: `aborted` says whether the answer was given
        // or the agent was stopped partway.
        Some("agent_settled") => {
            say.settled = Some(
                record
                    .get("aborted")
                    .and_then(|aborted| aborted.as_bool())
                    .unwrap_or(false),
            );
        }
        _ => {}
    }
    say
}

#[cfg(test)]
mod tests {
    mod pod {
        //! Tests for `app/pod.rs`: the template, the bounded capture, the
        //! prompt, and the pod loop with a trivial command.

        use super::super::*;
        use crate::app::tests::support::*;
        use crossterm::event::KeyEventState;
        use std::io::Cursor;

        pub(super) fn pod_lines(app: &App) -> Vec<&str> {
            app.log
                .lines()
                .iter()
                .filter(|line| line.kind == "pod")
                .map(|line| line.text.as_str())
                .collect()
        }

        fn pod_command_configured() -> String {
            "/bin/echo {prompt}".to_owned()
        }

        /// The trivial command the process tests run, if this machine
        /// has it. A prerequisite checked at runtime, per the suite's
        /// convention: the process tests are the only ones that need it.
        pub(super) fn has(binary: &str) -> bool {
            std::path::Path::new(binary).exists()
        }

        /// Runs the test body from `dir` and puts the process working
        /// directory back on the way out, even through a panic, so the
        /// one relative-path test cannot move the rest of the suite.
        struct CurrentDir(std::path::PathBuf);

        impl CurrentDir {
            fn enter(dir: &std::path::Path) -> Self {
                let former = std::env::current_dir().expect("the working directory");
                std::env::set_current_dir(dir).expect("move to the folder");
                Self(former)
            }
        }

        impl Drop for CurrentDir {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }

        #[test]
        fn the_template_builds_arguments_without_a_shell() {
            let set = std::path::Path::new("/sets/night");
            let file = std::path::Path::new("/sets/night/first.strudel");
            let arguments = pod_arguments(
                "my-agent --file {file} --ask {prompt}",
                "make it funky",
                set,
                Some(file),
            )
            .expect("a template");
            assert_eq!(
                arguments,
                [
                    "my-agent",
                    "--file",
                    "/sets/night/first.strudel",
                    "--ask",
                    "make it funky"
                ],
                "the prompt keeps its spaces as one argument"
            );
            assert_eq!(
                pod_arguments("my-agent {set}", "go", set, None).unwrap(),
                ["my-agent", "/sets/night", "go"],
                "with no {{prompt}} placeholder the prompt is the final argument"
            );
            assert_eq!(
                pod_arguments("--prompt={prompt}", "a b", set, None).unwrap(),
                ["--prompt=a b"],
                "a placeholder inside a longer argument is replaced in place"
            );
            assert_eq!(
                pod_arguments("my-agent {prompt}", "rewrite {file} please", set, None).unwrap(),
                ["my-agent", "rewrite {file} please"],
                "the prompt's own words are not re-expanded"
            );
            assert_eq!(
                pod_arguments("", "anything", set, Some(file)),
                None,
                "an empty template is the feature off"
            );
        }

        #[test]
        fn the_capture_keeps_the_tail_within_its_caps() {
            let mut capture = Capture::default();
            for at in 0..CAPTURE_MAX_LINES + 40 {
                capture.push(format!("line {at}"));
            }
            assert_eq!(capture.lines.len(), CAPTURE_MAX_LINES, "the line cap holds");
            assert_eq!(capture.dropped, 40);
            assert_eq!(
                capture.lines.front().unwrap(),
                "line 40",
                "the tail is kept"
            );
            assert_eq!(capture.lines.back().unwrap(), "line 239");
            let before = capture.bytes;
            capture.push("x".repeat(CAPTURE_MAX_BYTES));
            assert!(
                capture.bytes <= CAPTURE_MAX_BYTES,
                "the byte cap holds: {} > {CAPTURE_MAX_BYTES}",
                capture.bytes
            );
            assert!(capture.bytes < before + CAPTURE_MAX_BYTES);
            assert!(capture.truncation_note().contains("dropped"));
        }

        #[test]
        fn a_long_line_is_cut_and_the_rest_read_away() {
            let (sender, receiver) = std::sync::mpsc::sync_channel(READ_QUEUE);
            let head = "a".repeat(CAPTURE_LINE_MAX);
            let tail = "b".repeat(CAPTURE_LINE_MAX * 2);
            let mut stream = Vec::new();
            stream.extend_from_slice(head.as_bytes());
            stream.extend_from_slice(tail.as_bytes());
            stream.push(b'\n');
            stream.extend_from_slice(b"after the long line\n");
            forward_stream(Cursor::new(stream), sender);
            let lines: Vec<String> = receiver.into_iter().collect();
            assert_eq!(lines.len(), 2, "the overlong line is one event");
            assert_eq!(
                lines[0],
                format!("{head} \u{2026}"),
                "the line is cut to its head and marked"
            );
            assert_eq!(lines[1], "after the long line");
        }

        /// A line of exactly the per-line bound is whole: the cut is for
        /// longer lines, and marking a complete line as cut would say
        /// something was dropped when nothing was.
        #[test]
        fn a_line_of_exactly_the_bound_is_not_cut() {
            let (sender, receiver) = std::sync::mpsc::sync_channel(READ_QUEUE);
            let mut stream = Vec::new();
            stream.extend_from_slice("a".repeat(CAPTURE_LINE_MAX).as_bytes());
            stream.push(b'\n');
            // The same line at the end of the stream, without a newline of
            // its own, is whole too.
            stream.extend_from_slice("b".repeat(CAPTURE_LINE_MAX).as_bytes());
            forward_stream(Cursor::new(stream), sender);
            let lines: Vec<String> = receiver.into_iter().collect();
            assert_eq!(
                lines,
                vec!["a".repeat(CAPTURE_LINE_MAX), "b".repeat(CAPTURE_LINE_MAX),]
            );
        }

        #[test]
        fn the_prompt_hides_until_a_command_is_set() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            let score = app.editor().source();
            let chord = |app: &mut App| {
                press(
                    app,
                    KeyCode::Char('j'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                );
            };
            chord(&mut app);
            assert!(app.pod_prompt.is_none(), "the prompt stays hidden");
            assert!(app.status.contains("no pod command"), "{}", app.status);

            app.ui_settings.pod_command = pod_command_configured();
            chord(&mut app);
            assert!(app.pod_prompt.is_some(), "the prompt opens");
            assert!(
                app.status.contains("Enter runs /bin/echo"),
                "{}",
                app.status
            );
            type_text(&mut app, "make it funky");
            assert_eq!(
                app.pod_prompt.as_ref().expect("the prompt").text,
                "make it funky",
                "typing lands in the prompt"
            );
            assert_eq!(app.editor().source(), score, "none of it reached the score");
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.pod_prompt.is_none());
            assert_eq!(
                app.editor().source(),
                score,
                "the score is untouched throughout"
            );
        }

        #[test]
        fn an_empty_request_keeps_the_prompt_and_a_paste_lands_in_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.pod_command = pod_command_configured();
            app.open_pod_prompt(PodPromptKind::Request);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.pod_prompt.is_some(),
                "an empty submit is not a request for nothing"
            );
            app.handle_terminal_event(Event::Paste("make it\nfunkier".into()))
                .expect("paste handled");
            assert_eq!(
                app.pod_prompt.as_ref().expect("the prompt").text,
                "make it funkier",
                "a multi-line paste becomes one line"
            );
            // Backspace and the arrows move inside the prompt alone.
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            assert_eq!(
                app.pod_prompt.as_ref().expect("the prompt").text,
                "make it funkie"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.pod_prompt.is_none());
        }

        /// Only keys and pastes are the prompt's. The mouse, a resize and
        /// focus loss pass through to whatever is beneath - the find bar
        /// and the quick action pass the same events on - while a key
        /// release stays the prompt's, so it cannot fall through and
        /// reach the surfaces under it.
        #[test]
        fn non_key_events_pass_through_beneath_the_prompt() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.pod_command = pod_command_configured();
            app.open_pod_prompt(PodPromptKind::Request);
            let mouse = Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            });
            assert!(
                !app.pod_prompt_event(&mouse),
                "the mouse is not the prompt's"
            );
            assert!(
                !app.pod_prompt_event(&Event::Resize(80, 24)),
                "nor a resize"
            );
            assert!(!app.pod_prompt_event(&Event::FocusLost), "nor focus loss");
            let release = Event::Key(KeyEvent {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Release,
                state: KeyEventState::NONE,
            });
            assert!(
                app.pod_prompt_event(&release),
                "a key release is consumed, not passed on"
            );
            assert_eq!(
                app.pod_prompt.as_ref().expect("the prompt").text,
                "",
                "nothing in the release reached the text"
            );
            let press = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            assert!(app.pod_prompt_event(&press), "a key press is the prompt's");
            assert_eq!(app.pod_prompt.as_ref().expect("the prompt").text, "x");
        }

        /// The window the prompt's one row shows is laid out in display
        /// cells: wide characters still fit, and the caret column is
        /// where the terminal draws the caret, not the character count.
        #[test]
        fn the_prompts_window_is_laid_out_in_cells() {
            let mut prompt = PodPrompt {
                kind: PodPromptKind::Request,
                text: String::new(),
                cursor: 0,
                selected: false,
            };
            prompt.insert("夜に夜に");
            let (shown, caret) = prompt.window(20);
            assert_eq!(shown, "夜に夜に");
            assert_eq!(caret, 8, "each wide character holds two cells");
            // A window narrower than the text keeps the caret's own tail:
            // one whole wide character fits before the caret's column,
            // and the one before it is left out rather than half-drawn.
            let (shown, caret) = prompt.window(4);
            assert_eq!(shown, "に");
            assert_eq!(caret, 2, "the caret column is inside the row");
        }

        #[test]
        fn the_command_prompt_edits_and_keeps_the_template() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.pod_command = pod_command_configured();
            app.open_pod_prompt(PodPromptKind::Command);
            // The template is one selection: the first key replaces it.
            type_text(&mut app, "/bin/echo {prompt}");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.ui_settings.pod_command, "/bin/echo {prompt}");
            assert_eq!(
                app.prefs.pod_command.as_deref(),
                Some("/bin/echo {prompt}"),
                "the template reaches the preferences"
            );
            assert!(app.pod_prompt.is_none(), "Enter closes the prompt");
        }

        /// Enter on the settings row opens the command prompt over the
        /// sheet, and the sheet is still there when it closes.
        #[test]
        fn the_settings_row_opens_the_command_prompt_over_the_sheet() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.toggle_settings_sheet();
            let sheet = app.settings_sheet.as_mut().expect("the sheet");
            sheet.selected = crate::settings::pod_command_row();
            app.handle_settings_key(KeyCode::Enter, false, false);
            let prompt = app.pod_prompt.as_ref().expect("the command prompt");
            assert_eq!(prompt.kind, PodPromptKind::Command);
            assert!(app.settings_sheet.is_some(), "the sheet stays open");
            type_text(&mut app, "/bin/echo {prompt}");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.ui_settings.pod_command, "/bin/echo {prompt}");
            assert!(
                app.settings_sheet.is_some(),
                "the sheet is where Esc comes back to"
            );
        }

        /// A trivial command runs, its lines reach the log, and its exit
        /// is reported once.
        #[cfg(unix)]
        #[test]
        fn a_trivial_pod_runs_finishes_and_is_said_in_the_log() {
            if !has("/bin/echo") {
                return;
            }
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.pod_command = "/bin/echo hello {prompt}".into();
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(&mut app, "pod");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let run = app.pod_run.as_ref().expect("the pod runs");
            assert_eq!(run.program, "/bin/echo");
            assert!(
                app.background_jobs()
                    .iter()
                    .any(|job| job.name == "pod /bin/echo")
            );

            for _ in 0..100 {
                app.pump_pod();
                if app.pod_run.is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(app.pod_run.is_none(), "the pod was never reaped");
            let lines = pod_lines(&app);
            assert!(
                lines.iter().any(|line| line.contains("started /bin/echo")),
                "the log says it started: {lines:?}"
            );
            assert!(
                lines.iter().any(|line| line.contains("hello pod")),
                "the pod's line reached the log: {lines:?}"
            );
            assert!(
                app.status.contains("finished"),
                "the footer says how it ended: {}",
                app.status
            );
        }

        /// A set opened by a relative path hands the pod absolute `{set}`
        /// and `{file}` anyway: the pod runs with the set folder as its
        /// working directory, so the relative spelling the studio was
        /// started with would resolve against the set folder instead of
        /// the folder the studio opened.
        #[cfg(unix)]
        #[test]
        fn a_relative_set_hands_the_pod_absolute_paths() {
            if !has("/bin/echo") {
                return;
            }
            let parent = tempfile::tempdir().expect("temp dir");
            std::fs::create_dir(parent.path().join("set")).expect("the set folder");
            let _dir = CurrentDir::enter(parent.path());
            let (mut app, _) = plain_app(std::path::Path::new("set"), "$: s(\"bd\")\n");
            assert!(
                app.scenes.directory().is_relative(),
                "the fixture must open the set by a relative path"
            );
            app.ui_settings.pod_command = "/bin/echo {set} {file}".into();
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(&mut app, "go");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            for _ in 0..100 {
                app.pump_pod();
                if app.pod_run.is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(app.pod_run.is_none(), "the pod was never reaped");
            let set = parent.path().join("set");
            let line = format!("{} {}", set.display(), set.join("first.strudel").display());
            let lines = pod_lines(&app);
            assert!(
                lines.iter().any(|said| said.contains(&line)),
                "the pod said the absolute paths, not the relative spelling: {lines:?}"
            );
        }

        /// A second prompt is refused while a pod runs, and the running
        /// pod is left to finish on its own.
        #[cfg(unix)]
        #[test]
        fn a_running_pod_refuses_a_second_one() {
            if !has("/bin/sleep") {
                return;
            }
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.pod_command = "/bin/sleep {prompt}".into();
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(&mut app, "30");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.pod_run.is_some(), "the pod runs");

            app.open_pod_prompt(PodPromptKind::Request);
            assert!(
                app.pod_prompt.is_none(),
                "the prompt is refused while a pod runs"
            );
            assert!(app.status.contains("already running"), "{}", app.status);

            // The test's own way out: the sleeping pod is killed rather
            // than left to hold the machine's process table for half a
            // minute.
            if let Some(run) = app.pod_run.take()
                && let PodBody::Command { mut child, .. } = run.body
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        /// The whole loop: the pod rewrites the score file, and the sync
        /// watch takes the new text into the editor.
        #[cfg(unix)]
        #[test]
        fn the_pod_rewrites_the_score_and_the_watch_follows() {
            if !has("/bin/cp") {
                return;
            }
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.external_changes = ExternalChanges::Sync;
            // The "pod": cp, with the prompt naming the file to copy over
            // the score. A real pod writes the file itself; cp is the
            // smallest program that does the same thing to the same end.
            let rewritten = directory.path().join("rewritten.strudel");
            std::fs::write(&rewritten, "$: s(\"bd sd\") // the pod's edit\n")
                .expect("the rewritten score");
            app.ui_settings.pod_command = "/bin/cp {prompt} {file}".into();
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(&mut app, rewritten.to_str().expect("a utf-8 path"));
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.pod_run.is_some(), "the pod runs");

            for _ in 0..50 {
                app.pump_turn();
                if app.editor().source().contains("the pod's edit") {
                    break;
                }
                std::thread::sleep(crate::app::external_changes::POLL);
            }
            assert_eq!(
                app.editor().source(),
                "$: s(\"bd sd\") // the pod's edit\n",
                "the watch never adopted the pod's file"
            );
        }
    }

    mod llm {
        //! Tests for the llm pod: the request it builds, the loop through
        //! a canned endpoint, the pod kinds' routing, and the key's
        //! precedence.

        use super::super::*;
        use super::pod::{has, pod_lines};
        use crate::app::tests::support::*;
        use std::io::{Read as _, Write as _};
        use std::net::{TcpListener, TcpStream};

        /// Serves one HTTP request on a background thread and answers it
        /// with `response`, a complete HTTP/1.1 reply. Mirrors the one-shot
        /// server of the pod-llm crate's own tests: the request the studio
        /// sent comes back for header and body assertions.
        fn serve_once(response: String) -> (String, std::thread::JoinHandle<String>) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
            let address = listener.local_addr().expect("read the listener's address");
            let server = std::thread::Builder::new()
                .name("mock-openai-server".into())
                .spawn(move || {
                    let (mut stream, _) = listener.accept().expect("the studio connects");
                    let request = read_request(&mut stream);
                    stream
                        .write_all(response.as_bytes())
                        .expect("write the canned reply");
                    request
                })
                .expect("spawn the mock server thread");
            (format!("http://{address}"), server)
        }

        /// Reads one request: the head, then exactly the body its
        /// `Content-Length` names, so the server never waits for a close
        /// the keep-alive client never sends.
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

        /// Wraps `body` in a complete HTTP/1.1 reply with the given status
        /// line.
        fn http_reply(status_line: &str, body: &str) -> String {
            format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }

        /// The fixture app over a score, with its set directory kept alive.
        fn llm_app() -> (App, tempfile::TempDir) {
            let directory = tempfile::tempdir().expect("temp dir");
            let (app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            (app, directory)
        }

        /// Configure the llm rows onto `app`.
        fn llm_configured(app: &mut App, base: &str, key: &str) {
            app.ui_settings.pod_kind = PodKind::Llm;
            app.ui_settings.llm_endpoint = base.to_owned();
            app.ui_settings.llm_model = "test-model".into();
            app.ui_settings.llm_api_key = key.into();
        }

        /// Submit `prompt` through the pod prompt, and pump until the pod
        /// it started is done.
        fn ask_and_finish(app: &mut App, prompt: &str) {
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(app, prompt);
            press(app, KeyCode::Enter, KeyModifiers::NONE);
            for _ in 0..400 {
                app.pump_turn();
                if app.pod_run.is_none() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("the pod never finished");
        }

        /// The request the mock server fielded, parsed as JSON: the body
        /// the studio sent.
        fn sent_body(request: &str) -> serde_json::Value {
            let body = request
                .split("\r\n\r\n")
                .nth(1)
                .expect("the request has a body");
            serde_json::from_str(body).expect("the request body is JSON")
        }

        #[test]
        fn the_request_names_the_score_and_the_change() {
            let message = llm_user_message("$: s(\"bd\")\n", "make it funky");
            assert_eq!(
                message, "The current score:\n\n$: s(\"bd\")\n\n\nThe change: make it funky",
                "the model is told what it is rewriting and how"
            );
        }

        /// The rows the llm pod needs are named one at a time, the way the
        /// command pod names its template; the agent rung refuses with
        /// nothing to configure.
        #[test]
        fn an_unconfigured_llm_pod_names_the_row_it_needs() {
            let (mut app, _directory) = llm_app();
            app.ui_settings.pod_kind = PodKind::Llm;
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(app.pod_prompt.is_none(), "nothing is configured");
            assert!(app.status.contains("no llm endpoint"), "{}", app.status);
            app.ui_settings.llm_endpoint = "http://127.0.0.1:9/v1".into();
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(app.status.contains("no llm model"), "{}", app.status);
            app.ui_settings.llm_model = "test-model".into();
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(app.status.contains("no llm key"), "{}", app.status);
            assert!(
                app.status.contains("RUSTEL_LLM_KEY"),
                "the refusal also names the environment variable: {}",
                app.status
            );
            app.ui_settings.llm_api_key = "stored-key".into();
            app.open_pod_prompt(PodPromptKind::Request);
            let prompt = app.pod_prompt.as_ref().expect("everything is set");
            assert_eq!(prompt.kind, PodPromptKind::Request);
            assert!(
                app.status.contains("Enter asks test-model"),
                "{}",
                app.status
            );

            // The agent kind has a command of its own, with a default:
            // cleared, the refusal names the row; kept, the prompt opens
            // ready to ask the default's first word.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            app.ui_settings.pod_kind = PodKind::Agent;
            app.ui_settings.agent_command.clear();
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(app.pod_prompt.is_none());
            assert!(app.status.contains("no agent command"), "{}", app.status);
            app.ui_settings.agent_command = crate::settings::AGENT_COMMAND_DEFAULT.into();
            app.open_pod_prompt(PodPromptKind::Request);
            let prompt = app.pod_prompt.as_ref().expect("the default configures it");
            assert_eq!(prompt.kind, PodPromptKind::Request);
            assert!(app.status.contains("Enter asks pi"), "{}", app.status);
        }

        /// The kind row routes the prompt: `llm` asks the endpoint and
        /// leaves the command template alone; `command` runs the template
        /// exactly as it did before the llm rows existed.
        #[cfg(unix)]
        #[test]
        fn the_kind_decides_which_pod_a_prompt_runs() {
            if !has("/bin/echo") {
                return;
            }
            let (mut app, _directory) = llm_app();
            let before = app.editor().source();
            app.ui_settings.pod_command = "/bin/echo hello {prompt}".into();
            llm_configured(&mut app, "http://127.0.0.1:1", "stored-key");
            ask_and_finish(&mut app, "go");
            let lines = pod_lines(&app);
            assert!(
                lines.iter().any(|line| line.contains("asking test-model")),
                "the llm pod ran: {lines:?}"
            );
            assert!(
                lines.iter().any(|line| line.contains("test-model failed")
                    && line.contains("the request failed")),
                "a refused port is a transport failure, said in the log: {lines:?}"
            );
            assert!(
                !lines.iter().any(|line| line.contains("started /bin/echo")),
                "the command template did not run while the kind was llm: {lines:?}"
            );
            assert_eq!(
                app.editor().source(),
                before,
                "a failed rewrite leaves the editor untouched"
            );

            // The command kind, unchanged: the template runs, its words
            // land in the log, and nothing of the llm rows is in the way.
            app.ui_settings.pod_kind = PodKind::Command;
            ask_and_finish(&mut app, "go");
            let lines = pod_lines(&app);
            assert!(
                lines.iter().any(|line| line.contains("started /bin/echo")),
                "the command pod ran: {lines:?}"
            );
            assert!(
                lines.iter().any(|line| line.contains("hello go")),
                "the template kept its placeholders: {lines:?}"
            );
        }

        /// The whole Mode A loop: submit, the rewrite goes out with the
        /// system prompt, the current score and the typed prompt; the
        /// returned score replaces the editor's text, its file takes it,
        /// and it runs like an update - with the external-changes watch
        /// off, because the pane is the studio's own.
        #[test]
        fn the_llm_pod_rewrites_the_file_and_the_score_runs() {
            // The reply arrives with the newline a model adds; the core's
            // cleanup trims it, so what lands is the bare score.
            let rewritten = "$: s(\"bd*2\") // the pod's edit";
            let (base, server) = serve_once(openai_reply(&format!("{rewritten}\n")));
            let (mut app, directory) = llm_app();
            assert_eq!(
                app.ui_settings.external_changes,
                ExternalChanges::Off,
                "the fixture must prove the studio-internal path alone"
            );
            llm_configured(&mut app, &base, "stored-key");
            ask_and_finish(&mut app, "make it funkier");

            // What went out: the stored key in the bearer header, the
            // core's system prompt, the active tab's score and the typed
            // change.
            let request = server.join().expect("the mock server thread ends");
            assert!(
                request.contains("Authorization: Bearer stored-key"),
                "the key travels in the bearer header: {request}"
            );
            assert!(
                request.starts_with("POST /chat/completions HTTP/1.1"),
                "the request went to the chat completions path: {request}"
            );
            let sent = sent_body(&request);
            assert_eq!(
                sent["messages"][0]["content"],
                rustel_pod_llm::STRUDEL_SYSTEM_PROMPT,
                "the system half is the core crate's constant"
            );
            let user = sent["messages"][1]["content"]
                .as_str()
                .expect("the user half is text");
            assert!(
                user.contains("$: s(\"bd\")"),
                "the active tab's current score went out: {user}"
            );
            assert!(
                user.contains("make it funkier"),
                "the typed prompt went out: {user}"
            );

            // And it runs like an update: the studio-internal evaluation
            // path queued the rewrite for the engine, with the watch off.
            // The queue is the proof the lib suite can hold - the engine's
            // own install is what the e2e suite exercises.
            let scene = app.scenes.current().id;
            let evaluation = app
                .pending_evaluation
                .iter()
                .chain(app.inflight.values())
                .find(|pending| pending.scene == scene)
                .expect("the rewrite was queued as an evaluation");
            assert_eq!(
                evaluation.source.as_ref(),
                rewritten,
                "the engine was sent the returned score"
            );
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains("external update")),
                "the update path ran, not just the watch's read: {:?}",
                app.log
                    .lines()
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                app.ui_settings.external_changes,
                ExternalChanges::Off,
                "and it needed no external-changes setting"
            );

            // The editor took the returned score, and its file did too.
            assert_eq!(app.editor().source(), rewritten);
            let file = directory.path().join("first.strudel");
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::fs::read_to_string(&file).ok().as_deref() != Some(rewritten) {
                app.pump_turn();
                assert!(
                    std::time::Instant::now() < deadline,
                    "the file never took the score"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let lines = pod_lines(&app);
            assert!(
                lines.iter().any(|line| line.contains("rewrote first")),
                "the log says what the pod did: {lines:?}"
            );
        }

        /// A failure keeps the editor and its file untouched, and lands in
        /// the log under the pod's kind, its message naming which failure
        /// it was.
        #[test]
        fn a_failed_rewrite_leaves_the_editor_untouched_and_says_why() {
            let (base, server) =
                serve_once(http_reply("HTTP/1.1 500 Internal Server Error", "boom"));
            let (mut app, directory) = llm_app();
            let before = app.editor().source();
            llm_configured(&mut app, &base, "stored-key");
            ask_and_finish(&mut app, "make it funkier");
            server.join().expect("the mock server thread ends");
            assert_eq!(
                app.editor().source(),
                before,
                "the editor text is untouched"
            );
            assert_eq!(
                std::fs::read_to_string(directory.path().join("first.strudel")).unwrap(),
                "$: s(\"bd\")\n",
                "the file is untouched too"
            );
            let lines = pod_lines(&app);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("HTTP 500") && line.contains("boom")),
                "the error names its kind and the endpoint's answer: {lines:?}"
            );
            assert!(
                app.status.contains("failed"),
                "the footer says how it ended: {}",
                app.status
            );
        }

        /// `RUSTEL_LLM_KEY` wins over the stored row: the environment is
        /// the way to hand the studio a key without writing it into the
        /// preferences file.
        #[test]
        fn the_environment_key_wins_over_the_stored_one() {
            // SAFETY: process-global environment, mutated only here, while
            // this test holds it; the suite runs one test at a time unless
            // told otherwise, and no other test reads this variable.
            unsafe { std::env::set_var(crate::settings::LLM_KEY_ENV, "env-key") };
            let (base, server) = serve_once(openai_reply("$: s(\"bd\")\n"));
            let (mut app, _directory) = llm_app();
            llm_configured(&mut app, &base, "stored-key");
            ask_and_finish(&mut app, "go");
            let request = server.join().expect("the mock server thread ends");
            unsafe { std::env::remove_var(crate::settings::LLM_KEY_ENV) };
            assert!(
                request.contains("Authorization: Bearer env-key"),
                "the environment's key went out: {request}"
            );
            assert!(
                !request.contains("stored-key"),
                "the stored row's key stayed home: {request}"
            );
        }

        /// The one-pod rule holds across kinds: while an llm rewrite is
        /// waiting for its endpoint, a second prompt is refused whatever
        /// kind would run it, and the running pod is left alone. No
        /// cancellation exists for it, so the test leaves the rewrite to
        /// its own sixty-second timeout on a socket the test never reads.
        #[test]
        fn a_running_llm_pod_refuses_a_second_one_of_any_kind() {
            // A bound listener that never accepts: the kernel's backlog
            // takes the connection, so the rewrite blocks on a reply that
            // never comes and the pod stays running for the whole test.
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
            let address = listener.local_addr().expect("read the listener's address");
            let (mut app, _directory) = llm_app();
            llm_configured(&mut app, &format!("http://{address}"), "stored-key");
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(&mut app, "go");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.pod_run.is_some(),
                "the llm pod is waiting on its endpoint"
            );

            // The same kind first, then the command kind: both meet the
            // one running pod, and neither queues behind it.
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(app.pod_prompt.is_none(), "a second llm prompt is refused");
            assert!(app.status.contains("already running"), "{}", app.status);
            app.ui_settings.pod_kind = PodKind::Command;
            app.ui_settings.pod_command = "/bin/echo {prompt}".into();
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(
                app.pod_prompt.is_none(),
                "a command prompt is refused while the llm pod runs"
            );
            assert!(
                app.status.contains("already running"),
                "the refusal names the running pod, not the kind: {}",
                app.status
            );
            assert!(app.pod_run.is_some(), "the running pod was left alone");
            // The reader is dropped with the app; the rewrite thread runs
            // out its timeout on its own and never blocks the suite.
        }

        /// Enter on the llm rows opens their own prompts over the sheet,
        /// the way the pod command row does, and what they keep reaches
        /// the preferences.
        #[test]
        fn the_llm_rows_edit_through_their_own_prompts_over_the_sheet() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.toggle_settings_sheet();
            let sheet = app.settings_sheet.as_mut().expect("the sheet");
            sheet.selected = crate::settings::llm_endpoint_row();
            app.handle_settings_key(KeyCode::Enter, false, false);
            let prompt = app.pod_prompt.as_ref().expect("the endpoint prompt");
            assert_eq!(prompt.kind, PodPromptKind::Endpoint);
            assert!(app.settings_sheet.is_some(), "the sheet stays open");
            type_text(&mut app, "http://127.0.0.1:9/v1");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.ui_settings.llm_endpoint, "http://127.0.0.1:9/v1");
            assert_eq!(
                app.prefs.llm_endpoint.as_deref(),
                Some("http://127.0.0.1:9/v1"),
                "the endpoint reaches the preferences"
            );
            assert!(
                app.settings_sheet.is_some(),
                "the sheet is where Esc comes back to"
            );

            // The key's prompt keeps the key out of the status line.
            let sheet = app.settings_sheet.as_mut().expect("the sheet");
            sheet.selected = crate::settings::llm_endpoint_row() + 2;
            app.handle_settings_key(KeyCode::Enter, false, false);
            assert_eq!(
                app.pod_prompt.as_ref().expect("the key prompt").kind,
                PodPromptKind::Key
            );
            type_text(&mut app, "sk-secret");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.ui_settings.llm_api_key, "sk-secret");
            assert!(
                !app.status.contains("sk-secret"),
                "the kept line never echoes the key: {}",
                app.status
            );
        }

        /// The kind row steps its ladder and keeps the choice, the way
        /// every ladder row does.
        #[test]
        fn the_kind_row_steps_its_ladder_and_persists() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.toggle_settings_sheet();
            let sheet = app.settings_sheet.as_mut().expect("the sheet");
            sheet.selected = crate::settings::pod_kind_row();
            app.handle_settings_key(KeyCode::Right, false, false);
            assert_eq!(app.ui_settings.pod_kind, PodKind::Llm);
            assert_eq!(app.prefs.pod_kind.as_deref(), Some("llm"));
            app.handle_settings_key(KeyCode::Right, false, false);
            assert_eq!(app.ui_settings.pod_kind, PodKind::Agent);
            assert_eq!(app.prefs.pod_kind.as_deref(), Some("agent"));
            app.handle_settings_key(KeyCode::Right, false, false);
            assert_eq!(app.ui_settings.pod_kind, PodKind::Command);
            assert_eq!(app.prefs.pod_kind.as_deref(), Some("command"));
            // An unreadable spelling reads as the default, not as an
            // error: the preferences file is hand-editable.
            assert_eq!(PodKind::parse("nonsense"), PodKind::Command);
        }
    }

    mod agent {
        //! Tests for the agent pod: the RPC loop against a fake agent -
        //! a tiny script standing in for the real `pi` - proving the
        //! spawn, the prompt command under its own id, the streamed
        //! events parsed into the capture, the process outliving its
        //! prompts, and the shutdown that ends it.

        use super::super::*;
        use super::pod::{has, pod_lines};
        use crate::app::tests::support::*;

        #[cfg(unix)]
        /// The fake agent script: one JSONL command per line on stdin,
        /// answered with a response record under the same id, two text
        /// deltas, and an agent_settled event - the shape a real agent's
        /// run takes. `$1` takes every line it reads, `$2` its process
        /// id, and `$3` picks a behaviour: `die` exits with status 3 as
        /// soon as the prompt lands (a crashed agent), `slow` waits ten
        /// seconds before answering (a prompt in flight), `linger`
        /// becomes a sleep that outlives its input (only the studio's
        /// shutdown can end it), `bye` answers and then exits 0 (an
        /// agent that completes its run and leaves, as any JSONL-RPC
        /// program may), and anything else answers and stays for the
        /// next prompt.
        fn fake_agent() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
            let home = tempfile::tempdir().expect("temp dir");
            let script = home.path().join("fake-agent");
            std::fs::write(
                &script,
                r##"#!/bin/sh
received=$1
pidfile=$2
echo $$ > "$pidfile"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$received"
  if [ "$3" = die ]; then exit 3; fi
  if [ "$3" = slow ]; then sleep 10; fi
  id=${line#*\"id\":\"}
  id=${id%%\"*}
  printf '{"id":"%s","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}\n' "$id"
  printf '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"the agent says "}}\n'
  printf '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"hello"}}\n'
  printf '{"type":"agent_settled","aborted":false}\n'
  if [ "$3" = bye ]; then exit 0; fi
done
printf 'eof\n' >> "$received"
if [ "$3" = linger ]; then exec sleep 300; fi
"##,
            )
            .expect("write the fake agent");
            make_executable(&script);
            let received = home.path().join("received");
            let pidfile = home.path().join("pid");
            (home, received, pidfile)
        }

        /// Make the script a program the studio can run, the way a real
        /// agent's launcher is. A prerequisite the caller checks at
        /// runtime, like every process test's binaries.
        #[cfg(unix)]
        fn make_executable(script: &std::path::Path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(script)
                .expect("the script exists")
                .permissions();
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(mode.mode() | 0o755))
                .expect("make the script runnable");
        }

        /// Whether the process `pid` is still running, straight from the
        /// kernel: the tests have no other handle on a process the studio
        /// took away.
        #[cfg(unix)]
        fn process_alive(pid: u32) -> bool {
            std::path::Path::new(&format!("/proc/{pid}")).exists()
        }

        /// Point the app's agent command at the fake agent and its files.
        #[cfg(unix)]
        fn agent_configured(
            app: &mut App,
            script: &std::path::Path,
            received: &std::path::Path,
            pidfile: &std::path::Path,
            mode: &str,
        ) {
            app.ui_settings.pod_kind = PodKind::Agent;
            app.ui_settings.agent_command = format!(
                "{} {} {} {mode}",
                script.display(),
                received.display(),
                pidfile.display()
            );
        }

        /// Submit `prompt` through the pod prompt.
        #[cfg(unix)]
        fn submit(app: &mut App, prompt: &str) {
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(app, prompt);
            press(app, KeyCode::Enter, KeyModifiers::NONE);
        }

        /// Pump until the prompt in flight is done.
        #[cfg(unix)]
        fn finish(app: &mut App) {
            for _ in 0..600 {
                app.pump_pod();
                if app.pod_run.is_none() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("the agent never settled");
        }

        /// The command lines the fake agent read, parsed as JSON in the
        /// order the studio sent them.
        #[cfg(unix)]
        fn sent_commands(received: &std::path::Path) -> Vec<serde_json::Value> {
            std::fs::read_to_string(received)
                .expect("the fake agent's file")
                .lines()
                .filter(|line| !line.trim().is_empty() && *line != "eof")
                .map(|line| serde_json::from_str(line).expect("a JSONL command line"))
                .collect()
        }

        /// The process id the fake agent wrote down when it started.
        #[cfg(unix)]
        fn pid_from(pidfile: &std::path::Path) -> u32 {
            std::fs::read_to_string(pidfile)
                .expect("the fake agent's pid")
                .trim()
                .parse()
                .expect("a process id")
        }

        /// The process tests' prerequisites: a shell to run the fake
        /// agent, and a /proc to ask whether a process is alive - checked
        /// at runtime, the way the suite checks the binaries its other
        /// process tests need.
        #[cfg(unix)]
        fn process_tests_possible() -> bool {
            has("/bin/sh") && process_alive(std::process::id())
        }

        /// The whole Mode B loop, without the real pi: the studio spawns
        /// the configured agent with a parseable command line, the prompt
        /// goes out as one RPC command under an id, the streamed events
        /// land in the capture as lines, the response under the same id
        /// is correlated rather than dumped in the log, and the process
        /// stays alive to take a second prompt - the accumulated context
        /// the kind exists for.
        #[cfg(unix)]
        #[test]
        fn the_agent_answers_under_its_prompt_ids_and_stays_alive() {
            if !process_tests_possible() {
                return;
            }
            let (home, received, pidfile) = fake_agent();
            let (mut app, _) = plain_app(home.path(), "$: s(\"bd\")\\n");
            agent_configured(
                &mut app,
                &home.path().join("fake-agent"),
                &received,
                &pidfile,
                "answer",
            );
            // The command template names a command pod, which must stay
            // home while the kind is agent.
            app.ui_settings.pod_command = "/bin/echo hello {prompt}".into();

            submit(&mut app, "make it funky");
            assert!(app.pod_run.is_some(), "the prompt is in flight");
            finish(&mut app);
            assert!(app.pod_agent.is_some(), "the process outlives the prompt");

            let lines = pod_lines(&app);
            assert!(
                lines.iter().any(|line| line.contains("as the agent")),
                "the log says the agent started: {lines:?}"
            );
            assert!(
                lines.contains(&"the agent says hello"),
                "the two deltas became one line in the capture: {lines:?}"
            );
            assert!(
                lines.iter().any(|line| line.contains("answered")),
                "settling finished the prompt: {lines:?}"
            );
            assert!(
                !lines
                    .iter()
                    .any(|line| line.contains("\"type\":\"response\"")),
                "the receipt was correlated under its id, not dumped raw: {lines:?}"
            );
            assert!(
                !lines
                    .iter()
                    .any(|line| line.contains("hello make it funky")),
                "the command template stayed home: {lines:?}"
            );

            // The command line the agent read: a parseable prompt command
            // with the text and the exact id its response answered.
            let sent = sent_commands(&received);
            assert_eq!(sent.len(), 1, "one prompt, one command line");
            assert_eq!(sent[0]["type"], "prompt");
            assert_eq!(sent[0]["message"], "make it funky");
            assert_eq!(sent[0]["id"], "pod-1", "the first prompt's id");

            // The second prompt goes to the same process, under the next
            // id: the context accumulates because the process never went.
            let pid = pid_from(&pidfile);
            assert!(process_alive(pid), "the agent is still running");
            submit(&mut app, "again");
            finish(&mut app);
            assert!(app.pod_agent.is_some(), "still the same agent");
            assert_eq!(pid_from(&pidfile), pid, "no fresh process was spawned");
            assert!(process_alive(pid), "the same process answered");
            let sent = sent_commands(&received);
            assert_eq!(sent.len(), 2, "the second prompt went out");
            assert_eq!(sent[1]["message"], "again");
            assert_eq!(sent[1]["id"], "pod-2", "the second prompt's own id");
            assert_eq!(
                pod_lines(&app)
                    .iter()
                    .filter(|line| line.contains("as the agent"))
                    .count(),
                1,
                "the agent was spawned once for both prompts"
            );
        }

        /// A crashed agent is said in the log, and the next submit spawns
        /// a fresh one: the crash costs the context, not the feature.
        #[cfg(unix)]
        #[test]
        fn a_dead_agent_is_said_and_the_next_prompt_starts_a_fresh_one() {
            if !process_tests_possible() {
                return;
            }
            let (home, received, pidfile) = fake_agent();
            let (mut app, _) = plain_app(home.path(), "$: s(\"bd\")\\n");
            agent_configured(
                &mut app,
                &home.path().join("fake-agent"),
                &received,
                &pidfile,
                "die",
            );

            submit(&mut app, "make it funky");
            finish(&mut app);
            assert!(app.pod_agent.is_none(), "the dead agent was taken away");
            let lines = pod_lines(&app);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("exited") && line.contains("status: 3")),
                "the log says how the agent died: {lines:?}"
            );
            assert!(
                app.status.contains("failed"),
                "the footer says the prompt failed: {}",
                app.status
            );

            // The next submit spawns a fresh one - which, in die mode,
            // dies the same way: what is proved here is the fresh
            // process taking the prompt, not its survival.
            let first = pid_from(&pidfile);
            submit(&mut app, "try again");
            finish(&mut app);
            let second = pid_from(&pidfile);
            assert_ne!(first, second, "the fresh agent is a new process");
            let lines = pod_lines(&app);
            assert_eq!(
                lines
                    .iter()
                    .filter(|line| line.contains("as the agent"))
                    .count(),
                2,
                "a fresh agent was spawned for the second prompt: {lines:?}"
            );
            // A fresh process counts its own ids from one.
            let sent = sent_commands(&received);
            assert_eq!(sent.len(), 2, "the second prompt reached the fresh agent");
            assert_eq!(sent[1]["message"], "try again");
            assert_eq!(sent[1]["id"], "pod-1", "a fresh process counts its own ids");
        }

        /// An agent that answers and then exits - `bye`, the way any
        /// JSONL-RPC program may leave after completing a run - finishes
        /// its prompt as answered, not failed: the exit is still said in
        /// the log and the dead process still taken away, but the answer
        /// is what the footer reports.
        #[cfg(unix)]
        #[test]
        fn an_agent_that_answers_and_exits_finishes_the_prompt_as_answered() {
            if !process_tests_possible() {
                return;
            }
            let (home, received, pidfile) = fake_agent();
            let (mut app, _) = plain_app(home.path(), "$: s(\"bd\")\\n");
            agent_configured(
                &mut app,
                &home.path().join("fake-agent"),
                &received,
                &pidfile,
                "bye",
            );

            submit(&mut app, "make it funky");
            finish(&mut app);
            assert!(app.pod_agent.is_none(), "the exited agent was taken away");
            assert!(
                app.status.contains("answered"),
                "the footer reports the answer, not the exit: {}",
                app.status
            );
            assert!(
                !app.status.contains("failed"),
                "the exit did not fail it: {}",
                app.status
            );
            let lines = pod_lines(&app);
            assert!(
                lines.contains(&"the agent says hello"),
                "the answer was logged: {lines:?}"
            );
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("exited") && line.contains("fresh one")),
                "the exit is still said in the log: {lines:?}"
            );

            // The next submit spawns a fresh one, exactly as after a
            // crash: what the exit costs is the context, not the feature.
            let first = pid_from(&pidfile);
            submit(&mut app, "try again");
            finish(&mut app);
            let second = pid_from(&pidfile);
            assert_ne!(first, second, "a fresh agent took the second prompt");
            let sent = sent_commands(&received);
            assert_eq!(sent.len(), 2, "the second prompt reached the fresh agent");
            assert_eq!(sent[1]["message"], "try again");
        }

        /// The agent dies with the studio: the shutdown closes the
        /// agent's stdin first - the protocol's own way down, which the
        /// lingering fake ignores - and what is still up after a moment
        /// is killed. Only the studio's shutdown can end that process.
        #[cfg(unix)]
        #[test]
        fn the_agent_dies_with_the_studio() {
            if !process_tests_possible() {
                return;
            }
            let (home, received, pidfile) = fake_agent();
            let (mut app, _) = plain_app(home.path(), "$: s(\"bd\")\\n");
            agent_configured(
                &mut app,
                &home.path().join("fake-agent"),
                &received,
                &pidfile,
                "linger",
            );
            submit(&mut app, "make it funky");
            finish(&mut app);
            let pid = pid_from(&pidfile);
            assert!(process_alive(pid), "the agent is up before the shutdown");

            // The studio goes out of scope: its Drop is the shutdown path.
            drop(app);
            let deadline = std::time::Instant::now() + Duration::from_secs(6);
            while process_alive(pid) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the studio's shutdown never ended the lingering agent"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            // The stdin close reached it on the way down - the orderly
            // half of the shutdown - and the kill ended it.
            assert!(
                std::fs::read_to_string(&received)
                    .expect("the fake agent's file")
                    .contains("eof"),
                "the shutdown closed the agent's stdin before killing it"
            );
        }

        /// A prompt while an agent is working is refused, the way every
        /// kind refuses a second pod: no queueing, no steering.
        #[cfg(unix)]
        #[test]
        fn a_running_agent_prompt_refuses_a_second_one() {
            if !process_tests_possible() {
                return;
            }
            let (home, received, pidfile) = fake_agent();
            let (mut app, _) = plain_app(home.path(), "$: s(\"bd\")\\n");
            agent_configured(
                &mut app,
                &home.path().join("fake-agent"),
                &received,
                &pidfile,
                "slow",
            );
            submit(&mut app, "make it funky");
            assert!(app.pod_run.is_some(), "the agent is working on it");

            app.open_pod_prompt(PodPromptKind::Request);
            assert!(app.pod_prompt.is_none(), "a second agent prompt is refused");
            assert!(app.status.contains("already running"), "{}", app.status);
            app.ui_settings.pod_kind = PodKind::Command;
            app.ui_settings.pod_command = "/bin/echo {prompt}".into();
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(
                app.pod_prompt.is_none(),
                "a command prompt is refused while the agent works"
            );
            assert!(app.pod_run.is_some(), "the working agent was left alone");
        }

        /// An agent command that cannot run is a failure said in the log,
        /// not a crash: the next submit with a working command tries
        /// again.
        #[cfg(unix)]
        #[test]
        fn an_agent_that_cannot_start_is_said_in_the_log() {
            if !has("/bin/sh") {
                return;
            }
            let home = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(home.path(), "$: s(\"bd\")\\n");
            app.ui_settings.pod_kind = PodKind::Agent;
            app.ui_settings.agent_command = "/no/such/agent --mode rpc".into();
            app.open_pod_prompt(PodPromptKind::Request);
            type_text(&mut app, "go");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.pod_run.is_none(), "nothing is running");
            assert!(app.pod_agent.is_none());
            assert!(
                app.status.contains("could not start the agent"),
                "{}",
                app.status
            );
            assert!(
                pod_lines(&app)
                    .iter()
                    .any(|line| line.contains("could not start /no/such/agent")),
                "the log names the program that would not run"
            );
        }

        /// The agent command row keeps its template the way the command
        /// row does: the default is the shipped `pi` line, Enter edits
        /// it over the sheet, and a cleared row refuses agent pods.
        #[test]
        fn the_agent_command_row_edits_and_keeps_its_template() {
            let home = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(home.path(), "$: s(\"bd\")\\n");
            assert_eq!(
                app.ui_settings.agent_command,
                crate::settings::AGENT_COMMAND_DEFAULT,
                "the row ships with the pi command line"
            );
            app.toggle_settings_sheet();
            let sheet = app.settings_sheet.as_mut().expect("the sheet");
            sheet.selected = crate::settings::agent_command_row();
            app.handle_settings_key(KeyCode::Enter, false, false);
            let prompt = app.pod_prompt.as_ref().expect("the agent command prompt");
            assert_eq!(prompt.kind, PodPromptKind::AgentCommand);
            assert!(app.settings_sheet.is_some(), "the sheet stays open");
            type_text(&mut app, "my-agent --mode rpc");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.ui_settings.agent_command, "my-agent --mode rpc");
            assert_eq!(
                app.prefs.agent_command.as_deref(),
                Some("my-agent --mode rpc"),
                "the template reaches the preferences"
            );

            // A cleared row is the agent pod turned off, and a request
            // says so.
            app.ui_settings.pod_kind = PodKind::Agent;
            app.ui_settings.agent_command.clear();
            app.open_pod_prompt(PodPromptKind::Request);
            assert!(app.pod_prompt.is_none());
            assert!(app.status.contains("no agent command"), "{}", app.status);
        }
    }
}
