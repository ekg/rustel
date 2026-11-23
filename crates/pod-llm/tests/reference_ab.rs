//! The reference-block A/B: one set of prompts, three system messages, one
//! live OpenAI-compatible gateway.
//!
//! The llm pod can carry the language reference at two sizes or not at all
//! ([`system_prompt`]). This harness answers the only question that matters
//! for that choice - does the reference change what the model writes - by
//! sending the same prompts with the instructions alone, with the one-line
//! sheet and with the full one, and scoring every reply mechanically:
//!
//! - the reply must parse as a score: the runtime's own lint
//!   ([`rustel_runtime::lint::lint`], the check `rustel check` runs before it
//!   evaluates) reports no syntax finding;
//! - every function-like name the reply calls must exist in the reference:
//!   the names are read from the same `cheat_sheet` the system message
//!   carries, and the reply is scanned for names followed by an argument
//!   list.
//!
//! The table the run prints is the evidence `docs/llm-reference-evidence.md`
//! reports. That document also owns the prompts: they are parsed from its
//! `ab-prompts` block, so the quoted prompts and the ones sent cannot drift
//! apart.
//!
//! Re-run, with the repository's product binary not required:
//!
//! ```sh
//! RUSTEL_LLM_KEY=<key> cargo test -p rustel-pod-llm --test reference_ab -- \
//!     --ignored --nocapture
//! ```
//!
//! Without `RUSTEL_LLM_KEY`, the run reads the pi agent's own store and takes
//! its `lunaroute.access` token (`$HOME/.pi/agent/auth.json`); it prints
//! neither. Replies land in `$RUSTEL_LLM_AB_DIR` (default
//! `<temp>/rustel-reference-ab`), one `<condition>--<prompt>.strudel` each, so
//! a reader can judge them and `rustel check` can run over them afterwards.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustel_pod_llm::{ContextSize, Endpoint, system_prompt};
use rustel_runtime::lint::{self, Level};
use rustel_runtime::reference::{SheetDetail, cheat_sheet};

/// The gateway the earlier llm stages used, and a model that answers inside
/// the pod's 2048-token reply budget.
const BASE_URL: &str = "https://gw.lunaroute.com/v1";
const MODEL: &str = "deepseek-4.1-flash";

/// The evidence document, which owns the prompts, and the fence that holds
/// them: `--- <name>` starts a prompt, `+++ <change>` ends its starting
/// score and names the change, and the lines between are the score.
const EVIDENCE_DOC: &str = "../../docs/llm-reference-evidence.md";
const PROMPT_FENCE: &str = "```ab-prompts";

/// The three system messages under test, in the order the studio's `llm
/// context` ladder steps through them.
const CONDITIONS: [(&str, Option<ContextSize>); 3] = [
    ("off", None),
    ("compact", Some(ContextSize::Compact)),
    ("full", Some(ContextSize::Full)),
];

/// JavaScript's own callables. A score may use them to compute values; they
/// are not Rustel names, so the sheet cannot list them.
const JS_CALLS: [&str; 12] = [
    "Array",
    "Boolean",
    "JSON",
    "Math",
    "Number",
    "Object",
    "String",
    "decodeURIComponent",
    "encodeURIComponent",
    "isNaN",
    "parseFloat",
    "parseInt",
];

/// JavaScript keywords that take a parenthesised operand, and the dynamic
/// `import(...)` a score may use to load a helper script.
const JS_KEYWORDS: [&str; 18] = [
    "await", "case", "catch", "delete", "do", "else", "for", "function", "if", "import", "in",
    "new", "of", "return", "switch", "typeof", "void", "while",
];

/// One prompt from the evidence document.
struct Prompt {
    name: String,
    /// The score the request starts from, as the user message quotes it.
    score: String,
    /// The change the prompt asks for.
    change: String,
}

/// One reply, scored.
struct Verdict {
    parses: bool,
    /// Every lint finding, `"<level> <message>"`, in the order the lint
    /// reported it.
    findings: Vec<String>,
    /// Called names the reference does not list, sorted and deduped.
    unknown: Vec<String>,
}

/// The live run: five prompts against the gateway once per reference size.
///
/// Ignored because it needs a credential and a network; the rest of this
/// file runs offline.
#[test]
#[ignore = "calls a live gateway; needs RUSTEL_LLM_KEY or the agent's auth.json"]
fn the_reference_block_changes_what_the_model_writes() {
    let key = api_key();
    let prompts = prompts(&Path::new(env!("CARGO_MANIFEST_DIR")).join(EVIDENCE_DOC));
    let known = reference_names();
    let endpoint = Endpoint::new(BASE_URL, MODEL, key.as_str());
    let out_dir = reply_dir();
    fs::create_dir_all(&out_dir).expect("the reply directory is writable");

    println!(
        "model {MODEL}, {} prompts, {} conditions, replies in {}",
        prompts.len(),
        CONDITIONS.len(),
        out_dir.display()
    );
    for (label, size) in CONDITIONS {
        let system = system_prompt(size, &[]);
        let first_user = user_message(&prompts[0]);
        let usage = measured_usage(&key, &system, &first_user);
        println!(
            "\n{label}: system {} bytes, {} input tokens for prompt 1 ({} cached)",
            system.len(),
            usage.0,
            usage.1
        );
        for prompt in &prompts {
            let user = user_message(prompt);
            let started = Instant::now();
            let reply = endpoint
                .rewrite(&system, &user)
                .unwrap_or_else(|error| panic!("{label}/{}: {error}", prompt.name));
            let elapsed = started.elapsed();
            let path = out_dir.join(format!("{label}--{}.strudel", prompt.name));
            fs::write(&path, &reply).expect("the reply is written");
            let verdict = verdict(&reply, &known);
            println!(
                "{label}\t{}\t{:.1}s\t{} bytes out\tparses={}\tunknown={:?}\tfindings={:?}",
                prompt.name,
                elapsed.as_secs_f64(),
                reply.len(),
                verdict.parses,
                verdict.unknown,
                verdict.findings
            );
        }
    }
}

/// The system messages differ in the way the A/B assumes: the instructions
/// alone are the constant, both sizes carry the sheet, and the full one adds
/// the parameter lines. Without this the comparison would measure nothing.
#[test]
fn the_three_system_messages_differ_only_in_the_reference() {
    let off = system_prompt(None, &[]);
    let compact = system_prompt(Some(ContextSize::Compact), &[]);
    let full = system_prompt(Some(ContextSize::Full), &[]);
    assert_eq!(off, rustel_pod_llm::STRUDEL_SYSTEM_PROMPT);
    for size in [&compact, &full] {
        assert!(size.starts_with(&off), "the instructions come first");
        assert!(size.contains("`lpf(frequency)`"), "the sheet lists lpf");
    }
    assert!(full.len() > compact.len());
    assert_eq!(
        compact,
        system_prompt(Some(ContextSize::Compact), &[]),
        "the message is a function of the build, never of the call"
    );
}

/// The prompts in the evidence document parse, and each names a score and a
/// change.
#[test]
fn the_evidence_document_lists_the_prompts() {
    let prompts = prompts(&Path::new(env!("CARGO_MANIFEST_DIR")).join(EVIDENCE_DOC));
    assert_eq!(prompts.len(), 5);
    for prompt in &prompts {
        assert!(!prompt.score.trim().is_empty(), "{}", prompt.name);
        assert!(!prompt.change.trim().is_empty(), "{}", prompt.name);
        assert!(
            prompt.score.lines().any(|line| line.starts_with("$:")),
            "{} starts from a lane",
            prompt.name
        );
    }
}

/// The name scan finds what it is for: a function Rustel installs is known, a
/// name the sheet lacks is not, and strings, comments, member calls of
/// declared names and JavaScript's own callables are not mistaken for names.
#[test]
fn the_name_scan_reports_only_names_the_reference_lacks() {
    let known = reference_names();
    assert!(known.contains("note") && known.contains("lpf"));
    assert!(known.contains("att"), "an alias is a name too");

    let clean = "$: s(\"bd*4\").lpf(200)\n// see `flurble`\nconst tune = (x) => x;\n";
    assert_eq!(verdict(clean, &known).unknown, Vec::<String>::new());
    assert!(verdict(clean, &known).parses);

    let guessed = "$: note(\"c\").flurble(0.5).gain(Math.round(0.5))\n";
    let scored = verdict(guessed, &known);
    assert_eq!(scored.unknown, vec!["flurble".to_owned()]);
    assert!(scored.parses, "an unknown name is not a syntax error");
    assert!(
        scored.findings.iter().any(|f| f.contains("flurble")),
        "the lint sees the same name: {:?}",
        scored.findings
    );

    let broken = "$: s(\"bd\"\n";
    assert!(!verdict(broken, &known).parses, "an unclosed call");
}

/// The key the run uses: `RUSTEL_LLM_KEY` when it is set, otherwise the pi
/// agent's own gateway token. The run prints it nowhere.
fn api_key() -> String {
    if let Ok(key) = std::env::var("RUSTEL_LLM_KEY")
        && !key.trim().is_empty()
    {
        return key.trim().to_owned();
    }
    let home = std::env::var("HOME").expect("HOME is set, or RUSTEL_LLM_KEY is");
    let path = PathBuf::from(home).join(".pi/agent/auth.json");
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "set RUSTEL_LLM_KEY: {} is unreadable ({error})",
            path.display()
        )
    });
    let store: serde_json::Value = serde_json::from_str(&text).expect("auth.json is JSON");
    store["lunaroute"]["access"]
        .as_str()
        .expect("auth.json carries lunaroute.access")
        .to_owned()
}

/// Where the replies land.
fn reply_dir() -> PathBuf {
    match std::env::var("RUSTEL_LLM_AB_DIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => std::env::temp_dir().join("rustel-reference-ab"),
    }
}

/// The user half of a rewrite request, worded exactly as the studio's and the
/// Android host's `llm_user_message` word it: the score, then the change.
fn user_message(prompt: &Prompt) -> String {
    format!(
        "The current score:\n\n{}\n\nThe change: {}",
        prompt.score.trim_end(),
        prompt.change.trim()
    )
}

/// The prompts, read from the evidence document's `ab-prompts` fence.
fn prompts(document: &Path) -> Vec<Prompt> {
    let text = fs::read_to_string(document)
        .unwrap_or_else(|error| panic!("{} is unreadable ({error})", document.display()));
    let block = text
        .split_once(PROMPT_FENCE)
        .unwrap_or_else(|| panic!("{} has a {PROMPT_FENCE} fence", document.display()))
        .1
        .split_once("\n```")
        .expect("the fence is closed")
        .0
        .trim_start_matches('\n');
    let mut prompts: Vec<Prompt> = Vec::new();
    for line in block.lines() {
        if let Some(name) = line.strip_prefix("--- ") {
            prompts.push(Prompt {
                name: name.trim().to_owned(),
                score: String::new(),
                change: String::new(),
            });
        } else if let Some(change) = line.strip_prefix("+++ ") {
            let prompt = prompts.last_mut().expect("a change belongs to a prompt");
            prompt.change = change.trim().to_owned();
        } else if let Some(prompt) = prompts.last_mut() {
            prompt.score.push_str(line);
            prompt.score.push('\n');
        }
    }
    prompts
}

/// Every name the sheet the system message carries lists, with each entry's
/// aliases. The three catalogues (`chord`, `scale`, `sound`) are words a
/// score writes inside a call, never called themselves, so they are not
/// names a reply can get wrong.
fn reference_names() -> BTreeSet<String> {
    let sheet = cheat_sheet(SheetDetail::Line);
    let mut names = BTreeSet::new();
    for line in sheet.lines() {
        let Some(rest) = line.strip_prefix("- `") else {
            continue;
        };
        let Some(end) = rest.find('`') else {
            continue;
        };
        let entry = &rest[..end];
        names.insert(entry.split('(').next().unwrap_or(entry).trim().to_owned());
        // The alias list sits between the signature's backtick and the
        // documentation's em dash: "- `attack(attack)` (also `att`) — ...".
        let tail = &rest[end + 1..];
        let segment = tail.split('\u{2014}').next().unwrap_or(tail);
        for alias in alias_names(segment) {
            names.insert(alias);
        }
    }
    names
}

/// The backticked names of one alias segment, if it is one.
fn alias_names(segment: &str) -> Vec<String> {
    let Some(rest) = segment.trim_start().strip_prefix("(also ") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for (index, part) in rest.split('`').enumerate() {
        if index % 2 == 1 {
            names.push(part.trim().to_owned());
        }
    }
    names
}

/// Scores one reply: does it parse, and does every name it calls exist in the
/// reference.
fn verdict(reply: &str, known: &BTreeSet<String>) -> Verdict {
    // A reply is score text, never whole-buffer mini-notation: the same
    // `mini` flag `check_score` gives `rustel check`.
    let diagnostics = lint::lint(reply, false, None);
    let findings: Vec<String> = diagnostics
        .iter()
        .map(|finding| format!("{:?} {}", finding.level, finding.message))
        .collect();
    Verdict {
        parses: !diagnostics.iter().any(|f| f.level == Level::Syntax),
        findings,
        unknown: unknown_calls(reply, known),
    }
}

/// The names a reply calls that the reference does not list.
///
/// A call is an identifier followed by `(`. Names the reply defines itself,
/// member calls of JavaScript's own objects (`Math.round`), JavaScript's
/// callables and keywords are not Rustel names and are skipped; member calls
/// of anything else are kept, because `.voicing()` is where a model puts a
/// guessed control as often as it does at the head of a chain.
fn unknown_calls(reply: &str, known: &BTreeSet<String>) -> Vec<String> {
    let code = without_literals(reply);
    let declared = declared_names(&code);
    let mut unknown = BTreeSet::new();
    let bytes = code.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !is_ident_start(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_ident_char(bytes[index]) {
            index += 1;
        }
        let name = &code[start..index];
        let mut after = index;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if after >= bytes.len() || bytes[after] != b'(' {
            continue;
        }
        if JS_CALLS.contains(&name) || JS_KEYWORDS.contains(&name) || declared.contains(name) {
            continue;
        }
        let mut before = start;
        while before > 0 && bytes[before - 1].is_ascii_whitespace() {
            before -= 1;
        }
        let member = before > 0 && bytes[before - 1] == b'.';
        if member && JS_CALLS.contains(&receiver(&code, before - 1).as_str()) {
            continue;
        }
        if !known.contains(name) {
            unknown.insert(name.to_owned());
        }
    }
    unknown.into_iter().collect()
}

/// The identifier a `.` at `dot` hangs off, so `Math.round(` can be told from
/// `.voicing(`.
fn receiver(code: &str, dot: usize) -> String {
    let bytes = code.as_bytes();
    let mut start = dot;
    while start > 0 && is_ident_char(bytes[start - 1]) {
        start -= 1;
    }
    code[start..dot].to_owned()
}

/// The names the reply itself defines: `const|let|var|function|class NAME`.
fn declared_names(code: &str) -> BTreeSet<String> {
    const KEYWORDS: [&str; 5] = ["const", "let", "var", "function", "class"];
    let mut names = BTreeSet::new();
    let bytes = code.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !is_ident_start(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_ident_char(bytes[index]) {
            index += 1;
        }
        if !KEYWORDS.contains(&&code[start..index]) {
            continue;
        }
        let mut next = index;
        while next < bytes.len() && bytes[next].is_ascii_whitespace() {
            next += 1;
        }
        let name_start = next;
        while next < bytes.len() && is_ident_char(bytes[next]) {
            next += 1;
        }
        if next > name_start {
            names.insert(code[name_start..next].to_owned());
        }
    }
    names
}

/// The reply with string bodies and comments blanked, so a sound name in
/// `s("piano")` or a name in a comment is never read as a call.
fn without_literals(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'"' | b'\'' | b'`' => {
                let quote = byte;
                out.push(b' ');
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        out.extend_from_slice(b"  ");
                        index += 2;
                        continue;
                    }
                    if bytes[index] == quote {
                        break;
                    }
                    out.extend_from_slice(if bytes[index] == b'\n' { b"\n" } else { b" " });
                    index += 1;
                }
                out.push(b' ');
                index += 1;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    out.push(b' ');
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                out.extend_from_slice(b"  ");
                index += 2;
                while index < bytes.len() {
                    if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        out.extend_from_slice(b"  ");
                        index += 2;
                        break;
                    }
                    out.extend_from_slice(if bytes[index] == b'\n' { b"\n" } else { b" " });
                    index += 1;
                }
            }
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).expect("blanking literals keeps the text valid")
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn is_ident_char(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

/// The gateway's own count of a request's input tokens and of the part it
/// served from its prefix cache.
///
/// The pod client hands back the assistant text alone, so this one request
/// goes out by hand: the number is what the provider bills, which is the
/// size the reference choice is really about.
fn measured_usage(key: &str, system: &str, user: &str) -> (u64, u64) {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(60))
        .timeout(Duration::from_secs(120))
        .build();
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "max_tokens": 16,
        "stream": false,
    });
    let response = agent
        .post(&format!("{BASE_URL}/chat/completions"))
        .set("Authorization", &format!("Bearer {key}"))
        .send_json(body)
        .expect("the gateway answers the measurement request");
    let reply: serde_json::Value = response.into_json().expect("the reply is JSON");
    (
        reply["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        reply["usage"]["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0),
    )
}
