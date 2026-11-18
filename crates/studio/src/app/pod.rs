//! The pod prompt, and the pod it starts: an external agent the player
//! configures, handed the score to rewrite.
//!
//! A pod is any command that takes a prompt and writes score files: an
//! AI agent CLI, a script of one's own, anything at all. The command is
//! a template in Settings ▸ Live coding - `my-agent --prompt {prompt}
//! {file}` - and the pod prompt is one line of text over the studio.
//! Submitting it spawns the command with the set folder as its working
//! directory, so a relative `{file}` or a pod that writes beside itself
//! lands in the set. No shell stands between the studio and the program:
//! the template is split on whitespace into `Command` arguments, and
//! nothing in it is quoted, escaped or expanded by anything else.
//!
//! What the pod writes, the [external changes
//! watch](external_changes.rs) follows: `sync` takes the saved text into
//! the editor, and `sync & eval` plays it. The pod's own words - both
//! streams, each line as it completes - go to the log under `pod`, and
//! the running pod is a row in the jobs sheet until it exits.
//!
//! At most one pod runs at a time, and a prompt submitted while one
//! runs is refused rather than queued: the pod owns the score file
//! while it works, and two of them writing one file is a race nobody
//! asked for. Cancellation is not offered for the same reason - killing
//! a pod mid-write leaves a half-written score, which is the worse way
//! to lose one. A pod that outlives the studio keeps running: nothing
//! kills it on the way out.

use super::*;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::path::Path;
use std::process::{Child, Stdio};

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

/// The most a prompt holds. A prompt is a sentence or two for the pod;
/// a book is a score, and belongs in the file the pod rewrites.
const MAX_REQUEST_CHARS: usize = 2000;

/// The most the command template holds. It is one command line with no
/// quoting, so this is well past anything usable.
const MAX_COMMAND_CHARS: usize = 500;

/// What the one pod prompt is asking for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PodPromptKind {
    /// A prompt to hand to the pod now, from the pod prompt's keybind.
    Request,
    /// The command template itself, from Enter on the settings row.
    Command,
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
    fn title(&self) -> &'static str {
        match self.kind {
            PodPromptKind::Request => " pod ",
            PodPromptKind::Command => " pod command ",
        }
    }

    /// The text the prompt's one row shows, and the caret's column in
    /// it: the tail that holds the caret, the way a terminal line reads.
    fn window(&self, width: usize) -> (String, u16) {
        let width = width.max(1);
        let caret = self.text[..self.cursor].chars().count();
        let start = caret.saturating_sub(width - 1);
        let shown: String = self.text.chars().skip(start).take(width).collect();
        (shown, (caret - start) as u16)
    }

    fn insert(&mut self, text: &str) {
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        let limit = match self.kind {
            PodPromptKind::Request => MAX_REQUEST_CHARS,
            PodPromptKind::Command => MAX_COMMAND_CHARS,
        };
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
/// one argument however many spaces its value holds. A template with no
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
                .replace("{prompt}", prompt)
                .replace("{set}", &set.display().to_string())
                .replace("{file}", file.as_deref().unwrap_or_default())
        })
        .collect();
    if !template.contains("{prompt}") {
        arguments.push(prompt.to_owned());
    }
    Some(arguments)
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

/// One pod, running: the child, what its readers have sent, and the
/// bounded tail of what has been read.
pub(super) struct PodRun {
    /// The program that is running, as the jobs row and the log name it.
    pub(super) program: String,
    receiver: std::sync::mpsc::Receiver<String>,
    child: Child,
    capture: Capture,
}

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
            .title(prompt.title())
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
fn forward_stream<R: std::io::Read>(mut reader: R, sender: std::sync::mpsc::Sender<String>) {
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
            if at >= CAPTURE_LINE_MAX {
                let head: Vec<u8> = pending.drain(..CAPTURE_LINE_MAX).collect();
                if send_line(&sender, head, true).is_err() {
                    return;
                }
                pending.drain(..=at - CAPTURE_LINE_MAX);
                continue;
            }
            let line: Vec<u8> = pending.drain(..=at).collect();
            if send_line(&sender, line, false).is_err() {
                // The studio is gone; the pipe is closed behind it anyway.
                return;
            }
        }
        // Still no newline: an overlong line is already underway, so its
        // head goes out now and whatever follows is thrown away until
        // its own newline ends it.
        if !discarding && pending.len() >= CAPTURE_LINE_MAX {
            let head: Vec<u8> = pending.drain(..CAPTURE_LINE_MAX).collect();
            if send_line(&sender, head, true).is_err() {
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
                    let _ = send_line(&sender, pending, false);
                }
                return;
            }
            Ok(count) => pending.extend_from_slice(&chunk[..count]),
        }
    }
}

/// One line as one event, without its newline, marked when it was cut.
fn send_line(
    sender: &std::sync::mpsc::Sender<String>,
    mut bytes: Vec<u8>,
    cut: bool,
) -> Result<(), std::sync::mpsc::SendError<()>> {
    while matches!(bytes.last(), Some(b'\n') | Some(b'\r')) {
        bytes.pop();
    }
    let mut line = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        line.push_str(" \u{2026}");
    }
    sender
        .send(line)
        .map_err(|_| std::sync::mpsc::SendError(()))
}

impl App {
    /// Open the pod prompt. A request needs a configured command and a
    /// free pod slot; the command edit is the settings row's own prompt
    /// and always opens, over the sheet it came from.
    pub(super) fn open_pod_prompt(&mut self, kind: PodPromptKind) {
        if kind == PodPromptKind::Request {
            if self.ui_settings.pod_command.trim().is_empty() {
                self.status =
                    "no pod command is set \u{b7} Settings \u{25b8} Live coding \u{25b8} pod command"
                        .into();
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
            PodPromptKind::Request => String::new(),
        };
        let cursor = text.len();
        self.pod_prompt = Some(PodPrompt {
            kind,
            text,
            cursor,
            selected: kind == PodPromptKind::Command,
        });
        self.status = match kind {
            PodPromptKind::Request => format!(
                "pod - type a prompt \u{b7} Enter runs {} \u{b7} Esc closes",
                self.ui_settings
                    .pod_command
                    .split_whitespace()
                    .next()
                    .unwrap_or("the pod")
            ),
            PodPromptKind::Command => {
                "pod command - {prompt} {set} {file} are replaced \u{b7} Enter keeps \u{b7} Esc closes"
                    .into()
            }
        };
        self.dirty_frame = true;
    }

    /// Keys and pastes while the pod prompt has the keyboard. `true`
    /// when it took them; the mouse still reaches whatever is beneath.
    ///
    /// The transport stays global, the way it does from the find bar:
    /// stopping the music works from a prompt. Quit is answered before
    /// this is offered.
    pub(super) fn pod_prompt_event(&mut self, terminal_event: &Event) -> bool {
        if self.pod_prompt.is_none() {
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
                        self.status = match prompt.kind {
                            PodPromptKind::Request => "pod prompt closed".into(),
                            PodPromptKind::Command => "pod command unchanged".into(),
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

    /// Enter on the prompt: keep the command template, or start the pod.
    fn submit_pod_prompt(&mut self, prompt: PodPrompt) {
        match prompt.kind {
            PodPromptKind::Command => self.keep_pod_command(prompt),
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
                self.launch_pod(&text);
            }
        }
    }

    /// Keep the template the prompt holds, the way the settings sheet
    /// keeps any other change.
    fn keep_pod_command(&mut self, prompt: PodPrompt) {
        let text = prompt.text.trim().to_owned();
        self.ui_settings.pod_command = text.clone();
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        self.status = if text.is_empty() {
            "pod command cleared \u{b7} the pod prompt is hidden until one is set".into()
        } else {
            format!("pod command kept \u{b7} {text}")
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
        let set = self.scenes.directory().to_path_buf();
        let file = self
            .scenes
            .current()
            .is_score()
            .then(|| self.scenes.current().path.clone());
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
                let (sender, receiver) = std::sync::mpsc::channel();
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
                    receiver,
                    child,
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

    /// Take what the running pod has said since the last turn into the
    /// log, and reap a pod that has exited once its readers have read
    /// the last of it, so no line is dropped on the way out.
    pub(super) fn pump_pod(&mut self) {
        let Some(mut run) = self.pod_run.take() else {
            return;
        };
        let mut drained = false;
        for _ in 0..LINES_PER_TURN {
            match run.receiver.try_recv() {
                Ok(line) => {
                    run.capture.push(line.clone());
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
        match run.child.try_wait() {
            // Exited, and everything it said has been read: report it.
            Ok(Some(status)) if drained => {
                let outcome = if status.success() {
                    "finished".to_owned()
                } else {
                    format!("failed ({status})")
                };
                let program = run.program.clone();
                self.log.push(
                    LogLevel::Info,
                    "pod",
                    format!("{program} {outcome}{}", run.capture.truncation_note()),
                );
                self.status = format!("pod {outcome}");
                self.dirty_frame = true;
            }
            // Still running, or exited with output still on its way.
            Ok(_) => self.pod_run = Some(run),
            Err(error) => {
                let program = run.program.clone();
                self.log.push(
                    LogLevel::Warn,
                    "pod",
                    format!("{program}: could not ask how it ended: {error}"),
                );
                self.dirty_frame = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod pod {
        //! Tests for `app/pod.rs`: the template, the bounded capture, the
        //! prompt, and the pod loop with a trivial command.

        use super::super::*;
        use crate::app::tests::support::*;
        use std::io::Cursor;

        fn pod_lines(app: &App) -> Vec<&str> {
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
        fn has(binary: &str) -> bool {
            std::path::Path::new(binary).exists()
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
            let (sender, receiver) = std::sync::mpsc::channel();
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
            if let Some(mut run) = app.pod_run.take() {
                let _ = run.child.kill();
                let _ = run.child.wait();
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
}
