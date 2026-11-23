//! The language reference as data, and the Markdown cheat sheet built from it.
//!
//! Every documented name is owned by the code that installs it: the host
//! surface, the controls, the combinator registry, the painters, the synth
//! voices, the chord and scale catalogues, the extensions, Kabelsalat and
//! Hydra. The studio's reference panel assembles those tables for a reader at
//! the keyboard; this module assembles the same tables with no UI, so the
//! `rustel reference` command and any other host can hand a language model the
//! documentation of the build it drives.
//!
//! The sheet depends on the build alone: no clock, no paths, no host facts,
//! and every list is sorted, so two renders of one build agree byte for byte.

mod painters;

pub use painters::PAINTER_REFERENCE_ENTRIES;

use std::collections::{BTreeMap, HashMap, HashSet};

/// How much of each entry a sheet prints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SheetDetail {
    /// One line per name: the signature and the one-line documentation.
    Line,
    /// The same line, then every parameter's type and description.
    Params,
}

/// The longest one-line documentation the sheet prints, in characters,
/// before the line ends in an ellipsis. A cheat sheet is read by a model
/// with a prompt budget; the studio's panel is where the whole entry goes.
const SUMMARY_CHARS: usize = 100;

/// The most parameters a signature line names. A painter takes its options
/// as one object, so its entry carries every option as a parameter.
const SIGNATURE_PARAMS: usize = 6;

/// Headings whose entries are a catalogue of names rather than documented
/// functions. Scores write these inside `chord("...")`, `scale("...")` and
/// `s("...")`, so the sheet lists them as words, not as prose.
const NAME_HEADINGS: [&str; 3] = ["chord", "scale", "sound"];

/// The Markdown cheat sheet of the language this build installs.
pub fn cheat_sheet(detail: SheetDetail) -> String {
    let mut sheet = String::with_capacity(96 * 1024);
    sheet.push_str(&header());
    for (heading, rows) in sections() {
        sheet.push_str(&format!("\n## {heading}\n\n"));
        for row in rows {
            sheet.push_str(&format!("- `{}`", row.signature()));
            if !row.aliases.is_empty() {
                let aliases = row
                    .aliases
                    .iter()
                    .map(|alias| format!("`{alias}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                sheet.push_str(&format!(" (also {aliases})"));
            }
            if !row.summary.is_empty() {
                sheet.push_str(&format!(" — {}", row.summary));
            }
            sheet.push('\n');
            if detail == SheetDetail::Params {
                for param in &row.params {
                    sheet.push_str(&format!(
                        "  - `{}`: {} — {}\n",
                        param.name,
                        param.r#type,
                        one_line(&param.description)
                    ));
                }
            }
        }
    }
    for (heading, names) in catalogues() {
        sheet.push_str(&format!("\n## {heading}\n\n"));
        sheet.push_str(&wrapped_names(&names));
        sheet.push('\n');
    }
    sheet
}

/// What Rustel is, how a score is written, and which build this sheet came
/// from: a model reading the list needs to know both what to write and that
/// the list is the whole surface of the engine running the score.
fn header() -> String {
    format!(
        "# Rustel language reference\n\
         \n\
         Rustel is a native Rust engine for the Strudel pattern language.\n\
         \n\
         A score is a list of `$:` statements, each writing a pattern to its\n\
         own lane: `$: s(\"bd sd\").gain(0.8)`. Strings inside pattern calls are\n\
         mini-notation - `\"bd [sd sd] <hh oh>\"` - and a pattern chains as\n\
         method calls.\n\
         \n\
         Engine version {}.\n\
         \n\
         Every name below is installed by this engine, and the parameters in\n\
         parentheses are what the call takes. The chord, scale and sound\n\
         catalogues are name lists: a chord symbol goes inside `chord(\"...\")`,\n\
         a scale name inside `scale(\"...\")`, and a sound name inside `s(\"...\")`.\n",
        crate::product::VERSION
    )
}

/// One documented name, ready for a line of the sheet.
struct Row {
    /// The heading (the entry's first tag) this name is filed under.
    heading: &'static str,
    /// The name as the engine installs it.
    name: String,
    /// Other spellings the engine answers to, sorted.
    aliases: Vec<String>,
    params: Vec<ParamRow>,
    /// The one-line documentation, already elided to [`SUMMARY_CHARS`].
    summary: String,
}

struct ParamRow {
    name: String,
    r#type: String,
    description: String,
}

impl Row {
    fn signature(&self) -> String {
        if self.params.is_empty() {
            return self.name.clone();
        }
        let named = self
            .params
            .iter()
            .take(SIGNATURE_PARAMS)
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let more = if self.params.len() > SIGNATURE_PARAMS {
            ", …"
        } else {
            ""
        };
        format!("{}({named}{more})", self.name)
    }
}

/// The documented names this build installs, grouped by heading.
///
/// The headings are sorted, and the names within a heading, so the sheet is
/// the same text on every run of one build whatever order the tables are
/// read in.
fn sections() -> Vec<(&'static str, Vec<Row>)> {
    let mut grouped: BTreeMap<&'static str, Vec<Row>> = BTreeMap::new();
    for row in rows() {
        grouped.entry(row.heading).or_default().push(row);
    }
    let mut sections = grouped.into_iter().collect::<Vec<_>>();
    for (_, rows) in &mut sections {
        rows.sort_by(|left, right| left.name.cmp(&right.name));
    }
    sections
}

/// The names of the chord, scale and sound catalogues, as words.
fn catalogues() -> Vec<(&'static str, Vec<String>)> {
    let entries: [(&'static str, &[rustel_core::reference::ReferenceEntry]); 3] = [
        ("chord", rustel_core::voicings::CHORD_REFERENCE),
        ("scale", rustel_core::tonaljs_scales::SCALE_REFERENCE),
        ("sound", rustel_voice::SOUND_REFERENCE),
    ];
    entries
        .iter()
        .map(|(heading, documented)| {
            let mut names = documented
                .iter()
                .flat_map(|entry| std::iter::once(entry.name).chain(entry.synonyms.iter().copied()))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            (*heading, names)
        })
        .collect()
}

/// One row for each name the engine installs out of the documented tables,
/// in installation order, and with a name documented twice keeping its first
/// entry - so `density` reads as the control, not the combinator of that name.
fn rows() -> Vec<Row> {
    let known = crate::lint::known_names();
    let spellings = installed_spellings(known);
    let mut rows: Vec<Row> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for mut row in documented() {
        if NAME_HEADINGS.contains(&row.heading) {
            continue;
        }
        let usable = known.contains(row.name.as_str())
            || row
                .aliases
                .iter()
                .any(|alias| known.contains(alias.as_str()));
        if !usable {
            continue;
        }
        if let Some(installed) = spellings.get(&row.name)
            && *installed != row.name
        {
            let documented = std::mem::replace(&mut row.name, installed.clone());
            row.aliases.retain(|alias| *alias != row.name);
            if !row.aliases.contains(&documented) {
                row.aliases.push(documented);
            }
        }
        row.aliases.sort();
        if !seen.insert(row.name.clone()) {
            continue;
        }
        rows.push(row);
    }
    rows
}

/// Every table that documents an installed name, in installation order: the
/// host surface, the extensions, the controls, then the combinators, and
/// after those the tables whose names are not pattern calls - the painters,
/// the synth voices, the chords and the scales - then the Kabelsalat graph and
/// the Hydra namespace.
///
/// The studio's reference panel loads exactly these tables, through
/// `Reference::load`; a table added there belongs here too.
fn documented() -> Vec<Row> {
    let mut documented = Vec::new();
    for entry in rustel_jsruntime::reference_entries() {
        documented.extend(rows_of(entry));
    }
    #[cfg(feature = "extensions")]
    for entry in rustel_ext::reference_entries() {
        documented.extend(rows_of(entry));
    }
    for entry in rustel_core::controls_generated::visible_reference_entries() {
        documented.extend(rows_of(entry));
    }
    let combinators = rustel_core::register::default_registry();
    for entry in combinators.visible_reference_entries() {
        documented.extend(rows_of(entry));
    }
    for entry in PAINTER_REFERENCE_ENTRIES {
        documented.extend(rows_of(entry));
    }
    for entry in rustel_voice::SOUND_REFERENCE {
        documented.extend(rows_of(entry));
    }
    for entry in rustel_core::voicings::CHORD_REFERENCE {
        documented.extend(rows_of(entry));
    }
    for entry in rustel_core::tonaljs_scales::SCALE_REFERENCE {
        documented.extend(rows_of(entry));
    }
    for entry in rustel_jsruntime::kabelsalat_reference_entries() {
        documented.extend(rows_of(entry));
    }
    #[cfg(feature = "hydra")]
    for entry in rustel_jsruntime::hydra_reference_entries() {
        documented.extend(rows_of(entry));
    }
    documented
}

/// The rows one documented entry contributes: the entry itself, and the
/// inline spellings of a painter.
fn rows_of(entry: &rustel_core::reference::ReferenceEntry) -> Vec<Row> {
    let documentation = if entry.summary.is_empty() {
        first_sentence(entry.description)
    } else {
        entry.summary
    };
    let mut rows = vec![Row {
        heading: entry.tags.first().copied().unwrap_or("other"),
        name: entry.name.to_owned(),
        aliases: entry
            .synonyms
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        params: params_of(entry),
        summary: truncated(documentation),
    }];
    rows.extend(inline_painters(entry));
    rows
}

fn params_of(entry: &rustel_core::reference::ReferenceEntry) -> Vec<ParamRow> {
    entry
        .params
        .iter()
        .map(|param| ParamRow {
            name: param.name.to_owned(),
            r#type: param.r#type.to_owned(),
            description: param.description.to_owned(),
        })
        .collect()
}

/// The painters a score can also call inline, `_pianoroll()` beside
/// `pianoroll()`, as the transpiler installs them. They share the renderer's
/// documentation; only the spelling and the placement differ.
fn inline_painters(entry: &rustel_core::reference::ReferenceEntry) -> Vec<Row> {
    if entry.tags.first() != Some(&"visualization") {
        return Vec::new();
    }
    std::iter::once(entry.name)
        .chain(entry.synonyms.iter().copied())
        .filter_map(|spelling| {
            let inline = format!("_{spelling}");
            rustel_transpiler::VISUAL_WIDGET_METHODS
                .contains(&inline.as_str())
                .then_some((spelling, inline))
        })
        .map(|(spelling, inline)| Row {
            heading: entry.tags.first().copied().unwrap_or("other"),
            name: inline,
            aliases: Vec::new(),
            params: params_of(entry),
            summary: format!("Inline {spelling} visualization below the call."),
        })
        .collect()
}

/// The spellings the engine installs for the names it documents in lower
/// case: `setcpm` is documented, `setCpm` is installed. The studio's reference
/// panel promotes the same way, so both show the name a score must write.
fn installed_spellings(known: &HashSet<String>) -> HashMap<String, String> {
    let mut camel = known
        .iter()
        .filter(|name| {
            name.starts_with(|c: char| c.is_ascii_lowercase())
                && name.chars().any(|c| c.is_uppercase())
        })
        .map(String::as_str)
        .collect::<Vec<_>>();
    // Sorted, so two spellings differing only in case resolve the same way
    // on every run.
    camel.sort();
    let mut spellings: HashMap<String, String> = HashMap::new();
    for name in camel {
        spellings
            .entry(name.to_lowercase())
            .or_insert_with(|| name.to_owned());
    }
    spellings
}

/// The first sentence of `text`, which is the whole of a one-sentence
/// description.
fn first_sentence(text: &str) -> &str {
    let end = text.find(['.', '\n']).map_or(text.len(), |end| end + 1);
    text.get(..end).unwrap_or(text)
}

/// `text` with runs of whitespace collapsed to one space, so a summary
/// written over several lines stays one line.
fn one_line(text: &str) -> String {
    let mut line = String::with_capacity(text.len());
    let mut pending_space = false;
    for word in text.split_whitespace() {
        if pending_space {
            line.push(' ');
        }
        line.push_str(word);
        pending_space = true;
    }
    line
}

/// `text` as one line of at most [`SUMMARY_CHARS`] characters, cut at a word
/// boundary and ended in an ellipsis when it does not fit.
fn truncated(text: &str) -> String {
    let line = one_line(text);
    if line.chars().count() <= SUMMARY_CHARS {
        return line;
    }
    let mut cut = line.chars().take(SUMMARY_CHARS).collect::<String>();
    if let Some(space) = cut.rfind(' ')
        && space * 2 > SUMMARY_CHARS
    {
        cut.truncate(space);
    }
    cut.push('…');
    cut
}

/// The most columns a word-list line fills before it wraps.
const WORD_LIST_COLUMNS: usize = 76;

/// `names` as comma-separated lines of at most [`WORD_LIST_COLUMNS`] columns.
fn wrapped_names(names: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for name in names {
        let column = line.chars().count();
        let width = name.chars().count();
        if column > 0 && column + 2 + width > WORD_LIST_COLUMNS {
            line.push(',');
            lines.push(std::mem::take(&mut line));
        } else if column > 0 {
            line.push_str(", ");
        }
        line.push_str(name);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> String {
        cheat_sheet(SheetDetail::Line)
    }

    #[test]
    fn the_sheet_documents_the_language_this_build_installs() {
        let sheet = sheet();
        assert!(sheet.starts_with("# Rustel language reference\n"));
        assert!(sheet.contains(crate::product::VERSION));
        assert!(sheet.contains("## temporal\n"));
        assert!(
            sheet.lines().filter(|line| line.starts_with("- `")).count() > 400,
            "the sheet lists the documented names"
        );
    }

    /// The same build must print the same bytes: a prompt cache, a committed
    /// transcript and a diff all depend on it.
    #[test]
    fn two_renders_of_one_build_agree_byte_for_byte() {
        for detail in [SheetDetail::Line, SheetDetail::Params] {
            assert_eq!(
                cheat_sheet(detail),
                cheat_sheet(detail),
                "{detail:?} is not stable"
            );
        }
    }

    /// Spellings worth pinning: a name the engine installs in camel case
    /// from a lowercase document, a painter's inline form, and a control
    /// whose parameter list is part of its contract.
    #[test]
    fn known_entries_appear_with_the_spellings_this_engine_installs() {
        let sheet = sheet();
        assert!(
            sheet.contains("- `setCpm(cpm)` (also `setcpm`) — Changes the global tempo"),
            "the installed camel-case spelling leads: {sheet:.2000}"
        );
        assert!(sheet.contains("- `_pianoroll(options, cycles"));
        assert!(sheet.contains("- `lpf(frequency)`"));
        assert!(sheet.contains("## visualization\n"));
    }

    /// Every listed name is one the engine answers to, by itself or through a
    /// spelling it lists, and every painter the transpiler recognises has its
    /// line.
    #[test]
    fn every_listed_name_is_installed_and_every_painter_is_listed() {
        let known = crate::lint::known_names();
        for row in rows() {
            let installed = known.contains(row.name.as_str())
                || row
                    .aliases
                    .iter()
                    .any(|alias| known.contains(alias.as_str()))
                || rustel_transpiler::VISUAL_WIDGET_METHODS.contains(&row.name.as_str());
            assert!(installed, "`{}` is not installed", row.name);
        }
        let sheet = sheet();
        for painter in rustel_transpiler::VISUAL_WIDGET_METHODS {
            // `tscope` and the other alternate spellings appear beside the
            // painter's own line rather than on one of their own.
            let named = format!("`{painter}(");
            let spelled = format!("`{painter}`");
            assert!(
                sheet.contains(&named) || sheet.contains(&spelled),
                "{painter} has no line"
            );
        }
    }

    /// The catalogues are words a score writes inside a call, not prose.
    #[test]
    fn the_catalogues_are_name_lists() {
        let sheet = sheet();
        let chord = sheet.find("## chord\n").expect("a chord list");
        let listed = &sheet[chord..];
        assert!(listed.contains(", ^7,"), "chord symbols are words");
        let scale = sheet.find("## scale\n").expect("a scale list");
        assert!(sheet[scale..].contains("major pentatonic"));
        let sound = sheet.find("## sound\n").expect("a sound list");
        assert!(sheet[sound..].contains("supersaw"));
    }

    /// The parameter detail is the whole size difference between the two
    /// sheets, and it carries each parameter's type.
    #[test]
    fn the_full_sheet_adds_the_parameters() {
        let line = sheet();
        let full = cheat_sheet(SheetDetail::Params);
        assert!(full.len() > line.len());
        assert!(full.contains("  - `frequency`: number | Pattern — "));
        assert!(!line.contains("  - `frequency`"));
    }

    #[test]
    fn a_summary_is_elided_at_a_word_boundary() {
        assert_eq!(truncated("short"), "short");
        let long = "word ".repeat(40);
        let cut = truncated(&long);
        assert!(cut.chars().count() <= SUMMARY_CHARS + 1, "{cut}");
        assert!(cut.ends_with('…'));
        assert!(!cut.contains("wor…"), "cut inside a word: {cut}");
        assert_eq!(one_line("two\nlines  here"), "two lines here");
        assert_eq!(first_sentence("One. Two."), "One.");
        assert_eq!(first_sentence("No stop"), "No stop");
    }
}
