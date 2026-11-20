//! The reference, read out of alea's own documentation.
//!
//! `assets/reference.md` is a copy of `docs/reference.md` from the alea
//! repository (`scripts/update-reference.sh` refreshes it). Rather than keep
//! a second list of primitives that would drift from the language, the IDE
//! reads its tables: what the reference says is what the IDE offers.

use std::collections::HashMap;
use std::sync::LazyLock;

const REFERENCE: &str = include_str!("../assets/reference.md");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// `work`, `let`, `render`…
    Keyword,
    /// `x`, `y`, `t`, `pi`…
    Builtin,
    /// Anything called with parentheses.
    Function,
    /// A named argument of a render: `width`, `palette`.
    Option,
}

impl Family {
    /// A letter for the completion list, in the spirit of an editor's icons.
    pub fn mark(self) -> &'static str {
        match self {
            Family::Keyword => "K",
            Family::Builtin => "·",
            Family::Function => "ƒ",
            Family::Option => ":",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// `blob(x, y, size, wobble, softness)`
    pub signature: String,
    pub summary: String,
    /// Where it is written down: `Shapes`, `render image`…
    pub section: String,
    pub family: Family,
}

impl Entry {
    pub fn params(&self) -> Vec<&str> {
        match self.signature.split_once('(') {
            Some((_, rest)) => rest.trim_end_matches(')').split(',').map(str::trim).filter(|p| !p.is_empty()).collect(),
            None => Vec::new(),
        }
    }
}

/// Statements get their summaries from the reference's own table, but their
/// names have to be named: `let name = expr` is not a call.
const KEYWORDS: &[(&str, &str, &str)] = &[
    (
        "work",
        "work Name(param = default) { … }",
        "One work per file: its statements, and the parameters another work can give it.",
    ),
    ("let", "let name = expr", ""),
    ("field", "field name = expr", ""),
    ("pattern", "pattern name = repeat N event(attrs)", ""),
    ("constraint", "constraint expr", ""),
    ("render", "render kind(args)", ""),
    ("output", "output expr", ""),
    ("grow", "grow over N for S { name = start then next }", ""),
    (
        "choose",
        "choose { a | b | c }",
        "Picks one branch at random, in proportion to its weight (1 when omitted), and evaluates only that one.",
    ),
    ("synth", "synth { … }", "An instrument written in alea, run once per sample of a note."),
    ("use", "use \"other.alea\"", "Brings in another work, which this one can then call by name."),
    ("repeat", "repeat N event(attrs)", "The events of a pattern: N of them, each with its own draws."),
    ("event", "event(attrs)", "One event of a pattern: its attributes are drawn once per event."),
    (
        "then",
        "then",
        "name = start then next, in a grow rule: what a field starts as, and what it becomes in the next generation.",
    ),
    ("for", "for", "[expr for name in list]: a list computed from another; also the number of generations in grow over N for S."),
    ("in", "in", "[expr for name in list]: names the element of the list being walked."),
    ("and", "and", "a and b: true when both are true."),
    ("or", "or", "a or b: true when either is true."),
];

pub fn catalogue() -> &'static [Entry] {
    static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(|| parse(REFERENCE));
    &ENTRIES
}

fn index() -> &'static HashMap<String, usize> {
    static INDEX: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
        let mut index: HashMap<String, usize> = HashMap::new();
        for (k, e) in catalogue().iter().enumerate() {
            match index.get(&e.name) {
                // `choose` is a form of the language and a list function:
                // the form is what someone typing `choose` means.
                Some(&seen) if catalogue()[seen].family <= e.family => {}
                _ => {
                    index.insert(e.name.clone(), k);
                }
            }
        }
        index
    });
    &INDEX
}

/// What the reference says about `name`, if anything.
pub fn find(name: &str) -> Option<&'static Entry> {
    index().get(name).map(|k| &catalogue()[*k])
}

/// Entries matching `query`, best first: the name it starts, then the name
/// it is in, then what the summary mentions.
pub fn search(query: &str) -> Vec<&'static Entry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return catalogue().iter().collect();
    }
    let mut found: Vec<(u8, &'static Entry)> = catalogue()
        .iter()
        .filter_map(|e| {
            let name = e.name.to_lowercase();
            let rank = if name == q {
                0
            } else if name.starts_with(&q) {
                1
            } else if name.contains(&q) {
                2
            } else if e.summary.to_lowercase().contains(&q) || e.section.to_lowercase().contains(&q) {
                3
            } else {
                return None;
            };
            Some((rank, e))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().map(|(_, e)| e).collect()
}

/// Reads the reference's tables.
fn parse(doc: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut keyword_rows: Vec<Entry> = Vec::new();
    let (mut part, mut section) = (String::new(), String::new());
    // The columns of the table being read, so a `Parameters` column is used
    // as such and a `Default` column is not mistaken for one.
    let mut columns: Vec<String> = Vec::new();

    for line in doc.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            part = title.trim().to_string();
            section = part.clone();
            columns.clear();
            continue;
        }
        if let Some(title) = line.strip_prefix("### ") {
            let title = title.trim();
            // Under Renders, a `###` names a render kind: `render image(…)`.
            section = if part == "Renders" { format!("render {title}") } else { title.to_string() };
            columns.clear();
            continue;
        }
        let Some(cells) = row(line) else {
            columns.clear();
            continue;
        };
        if cells.iter().all(|c| c.chars().all(|c| c == '-' || c == ':') && !c.is_empty()) {
            continue;
        }
        if columns.is_empty() {
            columns = cells.into_iter().map(|c| c.to_lowercase()).collect();
            continue;
        }

        if part == "Statements" {
            if let Some(written) = cells[0].trim().strip_prefix('`').and_then(|c| c.split('`').next()) {
                if let Some(name) = written.split_whitespace().next() {
                    keyword_rows.push(Entry {
                        name: name.to_string(),
                        signature: written.to_string(),
                        summary: clean(cells.get(1).copied().unwrap_or("")),
                        section: "Statements".into(),
                        family: Family::Keyword,
                    });
                }
            }
            continue;
        }
        let names = code_spans(cells[0]);
        if names.is_empty() {
            continue;
        }
        let tail = after_code(cells[0]);
        let rest = cells.get(1..).unwrap_or_default();
        let params_column = columns.get(1).is_some_and(|c| c.contains("parameter"));
        // `circle(x, y)` carries its own parameters; `meter` keeps them in
        // the next column.
        let shared: Vec<String> = if params_column { code_spans(rest.first().copied().unwrap_or("")) } else { Vec::new() };
        let mut parts: Vec<String> = tail.is_empty().then(Vec::new).unwrap_or_else(|| vec![tail.to_string()]);
        for (k, cell) in rest.iter().enumerate() {
            let column = columns.get(k + 1).map_or("", String::as_str);
            if cell.is_empty() || (params_column && k == 0) {
                continue;
            }
            // `Default`, `Range` and `Omitted` say what they are only in
            // their heading, which the panel does not show.
            match column {
                "default" | "range" | "omitted" => parts.push(format!("{column} {cell}")),
                _ => parts.push((*cell).to_string()),
            }
        }
        let summary = clean(&parts.join(" · "));

        for name in names {
            let (bare, signature) = match name.split_once('(') {
                Some((bare, _)) => (bare.trim().to_string(), name.clone()),
                None if !shared.is_empty() => (name.clone(), format!("{name}({})", shared.join(", "))),
                None => (name.clone(), name.clone()),
            };
            if bare.is_empty() || !bare.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
                continue;
            }
            let family = match () {
                _ if part == "Statements" => Family::Keyword,
                _ if part == "Lexical rules" => Family::Builtin,
                _ if signature.contains('(') => Family::Function,
                _ if section.starts_with("render ") => Family::Option,
                _ => continue,
            };
            // The first mention wins: `between` is defined once and
            // mentioned later. A fuller signature still upgrades it.
            let entry = Entry { name: bare.clone(), signature, summary: summary.clone(), section: section.clone(), family };
            match seen.get(&bare) {
                Some(k) if out[*k].params().len() >= entry.params().len() => {}
                Some(k) => out[*k] = entry,
                None => {
                    seen.insert(bare, out.len());
                    out.push(entry);
                }
            }
        }
    }

    for (name, signature, fallback) in KEYWORDS {
        let summary = match find_in(&keyword_rows, name) {
            Some(e) if !e.summary.is_empty() => e.summary.clone(),
            _ => (*fallback).to_string(),
        };
        let entry = Entry {
            name: (*name).to_string(),
            signature: (*signature).to_string(),
            summary,
            section: "Statements".into(),
            family: Family::Keyword,
        };
        out.push(entry);
    }
    out.sort_by(|a, b| a.family.cmp(&b.family).then_with(|| a.name.cmp(&b.name)));
    out
}

fn find_in<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.name == name)
}

/// The cells of a markdown table row, or `None` for anything else.
fn row(line: &str) -> Option<Vec<&str>> {
    let line = line.trim();
    let inner = line.strip_prefix('|')?.strip_suffix('|').unwrap_or(line.strip_prefix('|')?);
    Some(inner.split('|').map(str::trim).collect())
}

/// The backticked pieces of a cell, each kept only as far as its closing
/// parenthesis: `` `steps` (1–64) `` gives `steps`.
fn code_spans(cell: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = cell;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        let code = after[..end].trim();
        rest = &after[end + 1..];
        // Stop at the first piece that is prose rather than a name, so the
        // examples inside a description are not taken for entries.
        if code.is_empty() || code.contains(' ') && !code.contains('(') {
            continue;
        }
        let code = match code.split_once('(') {
            // `render sound(song)` is a sentence about a render, not a name.
            Some((name, _)) if name.trim().contains(char::is_whitespace) => continue,
            Some((name, args)) => match args.find(')') {
                Some(close) => format!("{}({}", name.trim(), &args[..=close]),
                None => continue,
            },
            None => code.to_string(),
        };
        if code.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
            out.push(code);
        }
    }
    out
}

/// What a cell says after its first code span: `` `blob(…)` — an irregular
/// round shape ``. Later code spans belong to the description.
fn after_code(cell: &str) -> &str {
    // A row can name several things at once — `abs(a)` `sign(a)` — and what
    // follows describes them all.
    let mut rest = cell.trim();
    while let Some(after) = rest.strip_prefix('`') {
        match after.find('`') {
            Some(close) => rest = after[close + 1..].trim_start(),
            None => return "",
        }
    }
    rest.trim().trim_start_matches(['—', '-', ':']).trim()
}

/// Markdown to plain text, since the panel shows one line per entry.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut link = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '*' => {}
            '[' => link = true,
            ']' if link => {
                link = false;
                // Skip the target of the link, keeping its text.
                if chars.peek() == Some(&'(') {
                    for c in chars.by_ref() {
                        if c == ')' {
                            break;
                        }
                    }
                }
            }
            c => out.push(c),
        }
    }
    let text = out.split_whitespace().collect::<Vec<_>>().join(" ");
    text.trim_start_matches([',', ';', '·', '—', '-', ':']).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primitives_are_read_with_their_parameters() {
        let blob = find("blob").expect("blob");
        assert_eq!(blob.signature, "blob(x, y, size, wobble, softness)");
        assert_eq!(blob.params(), ["x", "y", "size", "wobble", "softness"]);
        assert_eq!(blob.family, Family::Function);
        assert_eq!(blob.section, "Shapes");
        assert!(blob.summary.contains("irregular"), "{}", blob.summary);
        // A row that names several functions describes them all, and the
        // description starts after the last of the names.
        let abs = find("abs").expect("abs");
        assert!(abs.summary.starts_with("absolute value"), "{}", abs.summary);
        assert!(!blob.summary.contains('`'), "markdown left in: {}", blob.summary);
    }

    #[test]
    fn functions_distributions_and_music_are_all_there() {
        for (name, params) in [
            ("kaleidoscope", vec!["shape", "sides", "x", "y"]),
            ("smoothstep", vec!["e0", "e1", "a"]),
            ("between", vec!["a", "b"]),
            ("normal", vec!["m", "d"]),
            ("meter", vec!["tempo", "bars", "beats"]),
            ("euclid", vec!["hits", "steps", "rotate", "bars", "y"]),
        ] {
            let e = find(name).unwrap_or_else(|| panic!("{name} is missing"));
            assert_eq!(e.params(), params, "{name} has {}", e.signature);
            assert!(!e.summary.is_empty(), "{name} has no summary");
        }
    }

    #[test]
    fn render_options_know_which_render_they_belong_to() {
        let palette = find("palette").expect("palette");
        assert_eq!(palette.family, Family::Option);
        assert!(palette.section.starts_with("render "), "{}", palette.section);
        assert_eq!(find("duration").map(|e| e.section.as_str()), Some("render sound"));
    }

    #[test]
    fn keywords_and_builtins_are_offered_too() {
        assert_eq!(find("let").map(|e| e.family), Some(Family::Keyword));
        assert_eq!(find("choose").map(|e| e.signature.as_str()), Some("choose { a | b | c }"));
        let x = find("x").expect("x");
        assert_eq!(x.family, Family::Builtin);
        assert!(x.summary.contains("position"), "{}", x.summary);
    }

    #[test]
    fn nothing_absurd_got_in() {
        let all = catalogue();
        assert!(all.len() > 80 && all.len() < 400, "{} entries", all.len());
        for e in all {
            assert!(!e.summary.is_empty() || e.family == Family::Keyword, "{} says nothing", e.name);
            assert!(!e.name.is_empty() && !e.name.contains(' ') && !e.name.contains('`'), "{e:?}");
            assert!(e.signature.starts_with(&e.name), "{e:?}");
            assert!(e.summary.len() < 600, "{} has a whole paragraph", e.name);
        }
    }

    #[test]
    fn search_finds_by_name_then_by_meaning() {
        let found = search("kaleido");
        assert_eq!(found.first().map(|e| e.name.as_str()), Some("kaleidoscope"));
        assert!(search("noise").iter().any(|e| e.name == "clouds"), "clouds is smooth noise");
        assert!(search("").len() == catalogue().len());
        assert!(search("zzzz").is_empty());
    }
}
