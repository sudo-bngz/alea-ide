//! What to offer while a name is being typed.
//!
//! The work on screen is usually half-written, so nothing here parses: the
//! names a work binds are read with a tolerant scan, and the rest comes from
//! [`crate::reference`].

use std::ops::Range;

use crate::reference::{self, Entry, Family};

/// Names bound by the work itself are offered before anything else.
pub const LOCAL: &str = "•";

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub name: String,
    /// What goes into the text: a function brings its parenthesis, an
    /// option of a render its colon.
    pub insert: String,
    /// The signature, or where a local name was bound.
    pub detail: String,
    pub summary: String,
    pub mark: &'static str,
}

/// At most this many at once: a list to glance at, not to read.
const SHOWN: usize = 10;

/// The name being typed, ending at the cursor.
pub fn word_at(src: &str, cursor: usize) -> Range<usize> {
    let cursor = cursor.min(src.len());
    let start = src[..cursor].char_indices().rev().take_while(|(_, c)| is_name(*c)).last().map_or(cursor, |(i, _)| i);
    let end =
        cursor + src[cursor..].char_indices().take_while(|(_, c)| is_name(*c)).map(|(i, c)| i + c.len_utf8()).last().unwrap_or(0);
    start..end
}

fn is_name(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The names this work binds, and how each was written.
pub fn locals(src: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, how: &str| {
        // A work may name whatever it likes, except what the language
        // reserves: `t` is never a local, `size` may well be one.
        let reserved = reference::find(name).is_some_and(|e| matches!(e.family, Family::Builtin | Family::Keyword));
        if !name.is_empty() && !reserved && !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), how.to_string()));
        }
    };
    for line in src.lines() {
        let code = line.split("//").next().unwrap_or("").split('#').next().unwrap_or("");
        let mut words = code.split_whitespace().peekable();
        while let Some(word) = words.next() {
            match word {
                "let" | "field" | "pattern" => {
                    if let Some(next) = words.peek() {
                        // `let f(x) = …` binds f, and x inside it.
                        let (name, params) = next.split_once('(').unwrap_or((next, ""));
                        add(name.trim_end_matches('='), word);
                        for param in params.trim_end_matches([')', '=']).split(',') {
                            add(param.split('=').next().unwrap_or("").trim(), "parameter");
                        }
                    }
                }
                "for" => {
                    if let Some(next) = words.peek() {
                        add(next.trim(), "in a list");
                    }
                }
                "work" => {
                    if let Some(next) = words.peek() {
                        if let Some((_, params)) = next.split_once('(') {
                            for param in params.trim_end_matches([')', '{']).split(',') {
                                add(param.split('=').next().unwrap_or("").trim(), "parameter of the work");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// The render whose arguments the cursor sits in, if any: `render image(…`.
pub fn enclosing_render(src: &str, cursor: usize) -> Option<String> {
    let before = src.get(..cursor.min(src.len()))?;
    let mut depth = 0i32;
    let mut at = before.len();
    for (i, c) in before.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' if depth > 0 => depth -= 1,
            '(' => {
                // The name before this parenthesis, and what precedes it.
                let head = before[..i].trim_end();
                let name = head[word_at(head, head.len())].to_string();
                let kind = head[..head.len() - name.len()].trim_end();
                return kind.ends_with("render").then_some(name);
            }
            '\n' if at.saturating_sub(i) > 4000 => break,
            _ => {}
        }
        at = i;
    }
    None
}

/// What to offer for the name ending at `cursor`, best first.
pub fn candidates(src: &str, cursor: usize) -> Vec<Candidate> {
    let word = word_at(src, cursor);
    let prefix = src.get(word.start..cursor.min(src.len())).unwrap_or("").to_lowercase();
    // `prog.key` asks for a member, which the reference does not list.
    if src[..word.start].trim_end().ends_with('.') {
        return Vec::new();
    }
    let render = enclosing_render(src, word.start);
    let mut found: Vec<(u8, Candidate)> = Vec::new();

    for (name, how) in locals(src) {
        if let Some(rank) = rank(&name.to_lowercase(), &prefix) {
            let detail = format!("{how} {name}");
            let insert = name.clone();
            found.push((rank, Candidate { name, insert, detail, summary: String::new(), mark: LOCAL }));
        }
    }
    for entry in reference::catalogue() {
        let Some(rank) = rank(&entry.name.to_lowercase(), &prefix) else { continue };
        // An option belongs to its render: `palette` is offered inside
        // `render image(…)`, not in the middle of a shape.
        let rank = match entry.family {
            Family::Option if render.as_deref().is_some_and(|k| entry.section == format!("render {k}")) => rank,
            Family::Option if render.is_some() => rank + 1,
            Family::Option => continue,
            Family::Function => rank + 1,
            _ => rank + 2,
        };
        found.push((rank, candidate(entry, render.is_some())));
    }

    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())).then_with(|| a.1.name.cmp(&b.1.name)));
    found.dedup_by(|a, b| a.1.name == b.1.name);
    found.into_iter().take(SHOWN).map(|(_, c)| c).collect()
}

fn candidate(entry: &Entry, in_render: bool) -> Candidate {
    let insert = match entry.family {
        Family::Function => format!("{}(", entry.name),
        Family::Option if in_render => format!("{}: ", entry.name),
        _ => entry.name.clone(),
    };
    Candidate {
        name: entry.name.clone(),
        insert,
        detail: entry.signature.clone(),
        summary: entry.summary.clone(),
        mark: entry.family.mark(),
    }
}

/// How well a name answers what is typed: 0 the whole name, 1 its start, 2
/// somewhere inside it. Nothing typed offers everything.
fn rank(name: &str, prefix: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(1);
    }
    if name == prefix {
        Some(0)
    } else if name.starts_with(prefix) {
        Some(1)
    } else if name.contains(prefix) {
        Some(4)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<String> {
        let cursor = src.find('|').expect("put | where the cursor is");
        let src = src.replace('|', "");
        candidates(&src, cursor).into_iter().map(|c| c.name).collect()
    }

    #[test]
    fn the_word_being_typed_is_found() {
        let src = "let petal = blob(x: 1)";
        assert_eq!(word_at(src, 7), 4..9); // inside `petal`
        assert_eq!(&src[word_at(src, 16)], "blob");
        assert_eq!(word_at(src, 0), 0..3); // `let`, from its start
    }

    #[test]
    fn primitives_come_up_by_their_start() {
        let found = names("work W {\n  let a = kal|\n}");
        assert_eq!(found.first().map(String::as_str), Some("kaleidoscope"));
    }

    #[test]
    fn the_works_own_names_come_first() {
        let src = "work W {\n  let lace = ripples()\n  let petal = blob()\n  output la\n}";
        let first = candidates(src, src.find("la\n").unwrap() + 2).into_iter().next().unwrap();
        assert_eq!((first.name.as_str(), first.insert.as_str(), first.mark), ("lace", "lace", LOCAL));
        let found = names("work W {\n  let lace = ripples()\n  let petal = blob()\n  output la|\n}");
        assert!(!found.contains(&"petal".to_string()), "petal does not match `la`");
    }

    #[test]
    fn a_render_offers_its_own_options() {
        let found = names("work W {\n  render image(output, pal|)\n}");
        assert_eq!(found.first().map(String::as_str), Some("palette"));
        let src = "work W {\n  render image(output, pal)\n}";
        let inserted = candidates(src, src.find("pal)").unwrap() + 3);
        assert_eq!(inserted.first().map(|c| c.insert.as_str()), Some("palette: "));

        // Outside a render, an option of one is not a name to offer.
        assert!(!names("work W {\n  let a = blob(pal|)\n}").contains(&"palette".to_string()));
        // With nothing typed yet, an image render offers its own options
        // and not those of a sound.
        let both = names("work W {\n  render image(output, |)\n}");
        assert!(both.contains(&"palette".to_string()) && !both.contains(&"duration".to_string()), "{both:?}");
    }

    #[test]
    fn functions_bring_their_parenthesis_and_names_do_not() {
        let src = "work W {\n  let a = blo\n}";
        let blob = candidates(src, src.find("blo\n").unwrap() + 3).into_iter().next().unwrap();
        assert_eq!((blob.insert.as_str(), blob.detail.as_str()), ("blob(", "blob(x, y, size, wobble, softness)"));
        let src = "work W {\n  let a = t\n}";
        assert_eq!(candidates(src, src.find("t\n").unwrap() + 1).first().map(|c| c.insert.as_str()), Some("t"));
    }

    #[test]
    fn a_member_is_not_a_name_to_offer() {
        assert!(names("work W {\n  let k = prog.ke|\n}").is_empty());
    }

    #[test]
    fn half_written_works_still_offer_their_names() {
        // Nothing here parses: an unclosed call, and a work being typed.
        let src = "work W(size = 0.3) {\n  let lace = ripples(\n  let petal = blob(si|";
        let found = names(src);
        assert_eq!(found.first().map(String::as_str), Some("size"));
        assert!(found.contains(&"sign".to_string()), "{found:?}");
    }

    #[test]
    fn locals_never_shadow_the_language() {
        // `t` is reserved, `x` is a position: neither is a local name.
        let found = locals("work W {\n  let t = 1\n  let x = 2\n  let turns = 3\n}");
        assert_eq!(found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["turns"]);
    }

    #[test]
    fn the_render_around_the_cursor_is_recognised() {
        let src = "work W {\n  render image(merge(a, b), palette: \"ink\")\n}";
        assert_eq!(enclosing_render(src, src.find("palette").unwrap()), Some("image".into()));
        assert_eq!(enclosing_render(src, src.find("a, b").unwrap()), None, "inside merge, not the render");
        assert_eq!(enclosing_render(src, 5), None);
    }
}
