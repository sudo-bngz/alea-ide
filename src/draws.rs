//! The knobs of a work: every choice, range and number in the code, with the
//! slice of source each one owns.
//!
//! The code stays the truth. A control never holds a value of its own: it
//! reads one out of the text and writes one back, so moving a slider and
//! typing the number yourself are the same edit.

use std::ops::Range;

use alea::ast::{Arg, Expr, ExprKind, GrowRule, Stmt, Work};
use alea::error::Span;
use alea::{lexer, parser};

/// Distributions a range control can work on: `~between(a, b)` and friends.
/// Each is shown with the names its two numbers go by.
const SPREADS: &[(&str, [&str; 2])] = &[
    ("between", ["from", "to"]),
    ("uniform", ["from", "to"]),
    ("normal", ["around", "spread"]),
    ("around", ["around", "spread"]),
    ("beta", ["a", "b"]),
];

/// Past this many knobs the pane stops being a pane. Works this large are
/// edited in the code.
const LIMIT: usize = 80;

/// One number in the source: what it reads, and the exact slice it occupies.
#[derive(Clone, Debug, PartialEq)]
pub struct Num {
    pub value: f64,
    pub at: Range<usize>,
    /// Digits after the point in the source, so a rewrite keeps its shape.
    pub decimals: usize,
}

impl Num {
    /// The number written the way the source writes it.
    pub fn text(&self, value: f64) -> String {
        format!("{value:.*}", self.decimals)
    }

    /// How far dragging moves it per point: a hundredth of what it reads,
    /// so 240 moves in twos and 0.22 in thousandths. Never less than a
    /// quarter of its last digit, so the smallest written number still
    /// moves, and never so fast that a pull across the pane is a leap.
    pub fn speed(&self) -> f64 {
        let step = 10f64.powi(-(self.decimals as i32));
        (self.value.abs() / 100.0).max(step / 4.0)
    }
}

/// One option of a `choose`, as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Opt {
    pub text: String,
    pub at: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// `choose { a | b | c }`
    Choose {
        options: Vec<Opt>,
        /// Inside the braces, when every option is on one line: what `add`
        /// and `remove` need. `None` keeps those two out of reach.
        body: Option<Range<usize>>,
    },
    /// `~between(0.5, 0.8)`: a draw from a distribution with plain numbers.
    Spread { dist: String, per_point: bool, names: [&'static str; 2], from: Num, to: Num },
    /// A number written in the code.
    Number(Num),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Knob {
    /// The statement it belongs to: `petal`, `render image`.
    pub group: String,
    /// What it is called where it sits: an argument name, or the binding's.
    pub label: String,
    pub line: usize,
    /// Where the whole thing starts, for ordering and for jumping to it.
    pub at: usize,
    pub kind: Kind,
}

impl Knob {
    pub fn is_draw(&self) -> bool {
        !matches!(self.kind, Kind::Number(_))
    }
}

/// A rewrite of one slice of the source.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub at: Range<usize>,
    pub text: String,
}

/// Applies edits to `src`. They must not overlap.
pub fn apply(src: &mut String, mut edits: Vec<Edit>) {
    edits.sort_by_key(|e| std::cmp::Reverse(e.at.start));
    for e in edits {
        if e.at.end <= src.len() && src.is_char_boundary(e.at.start) && src.is_char_boundary(e.at.end) {
            src.replace_range(e.at, &e.text);
        }
    }
}

/// Adds an option to a `choose`, copying the last one so the edit is always
/// valid code; a multi-line choice gets a new line of its own.
pub fn add_option(src: &str, options: &[Opt], body: &Range<usize>) -> Option<Edit> {
    let last = options.last()?;
    let multiline = src.get(body.clone())?.contains('\n');
    let text = if multiline {
        let line = src[..last.at.start].rfind('\n').map_or(0, |n| n + 1);
        let indent: String = src[line..].chars().take_while(|c| *c == ' ').collect();
        format!("\n{indent}| {}", last.text)
    } else {
        format!(" | {}", last.text)
    };
    Some(Edit { at: last.at.end..last.at.end, text })
}

/// Removes one option, taking its separator with it.
pub fn remove_option(src: &str, options: &[Opt], at: usize) -> Option<Edit> {
    if options.len() < 2 {
        return None;
    }
    let gone = options.get(at)?;
    let at = match at {
        // The first option takes the bar that follows it.
        0 => gone.at.start..options[1].at.start,
        // Any other takes the bar before it.
        k => options[k - 1].at.end..gone.at.end,
    };
    let _ = src;
    Some(Edit { at, text: String::new() })
}

/// Every knob of `src`, in the order they are written. A work that does not
/// parse has none: the error is shown under the code instead.
pub fn knobs(src: &str) -> Vec<Knob> {
    let Ok(program) = lexer::lex(src).and_then(parser::parse) else {
        return Vec::new();
    };
    let mut walk = Walk { src, out: Vec::new() };
    walk.work(&program.work);
    walk.out.sort_by_key(|k| k.at);
    walk.out.truncate(LIMIT);
    number_the_repeats(&mut walk.out);
    walk.out
}

/// Five radii all called `outline` are told apart by their position.
fn number_the_repeats(knobs: &mut [Knob]) {
    let mut counts: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
    for k in knobs.iter() {
        *counts.entry((k.group.clone(), k.label.clone())).or_default() += 1;
    }
    let mut seen: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
    for k in knobs.iter_mut() {
        let key = (k.group.clone(), k.label.clone());
        if counts[&key] > 1 {
            let n = seen.entry(key).or_default();
            *n += 1;
            k.label = format!("{} {n}", k.label);
        }
    }
}

struct Walk<'a> {
    src: &'a str,
    out: Vec<Knob>,
}

impl Walk<'_> {
    fn work(&mut self, work: &Work) {
        for param in &work.params {
            if let Some(default) = &param.default {
                self.expr(default, &param.name, &param.name);
            }
        }
        for stmt in &work.stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { name, val, .. } | Stmt::Field { name, val, .. } => self.expr(val, name, name),
            Stmt::Func { name, body, .. } => self.expr(body, name, name),
            Stmt::Output { val, .. } => self.expr(val, "output", "output"),
            Stmt::Constraint { val, .. } => self.expr(val, "constraint", "constraint"),
            Stmt::Pattern { name, count, attrs, .. } => {
                self.expr(count, name, "count");
                self.args(attrs, name, name, name);
            }
            Stmt::Render { kind, args, .. } => self.args(args, &format!("render {kind}"), kind, kind),
            Stmt::Grow { size, steps, rules, .. } => {
                for (e, label) in [(size, "over"), (steps, "for")] {
                    if let Some(e) = e {
                        self.expr(e, "grow", label);
                    }
                }
                for GrowRule { name, start, next, .. } in rules {
                    self.expr(start, name, "start");
                    self.expr(next, name, "then");
                }
            }
        }
    }

    /// Names the arguments of a call: what they are called where given,
    /// else what the reference calls them, else what holds the call.
    fn args(&mut self, args: &[Arg], group: &str, call: &str, inherited: &str) {
        let params = crate::reference::find(call).map(|e| e.params()).unwrap_or_default();
        // One parameter is the thing itself (`floor(x)`): its name there
        // says less than the name of what is being written.
        let named_positions = params.len() >= 2;
        let mut position = 0;
        for arg in args {
            let label = match &arg.name {
                Some(name) => name.clone(),
                None => {
                    let from_reference = named_positions.then(|| params.get(position).copied()).flatten();
                    position += 1;
                    from_reference.unwrap_or(inherited).to_string()
                }
            };
            self.expr(&arg.val, group, &label);
        }
    }

    fn push(&mut self, group: &str, label: &str, span: Span, at: usize, kind: Kind) {
        self.out.push(Knob { group: group.to_string(), label: label.to_string(), line: span.line, at, kind });
    }

    fn expr(&mut self, e: &Expr, group: &str, label: &str) {
        match &e.kind {
            ExprKind::Num(value) => {
                if let Some(at) = range(self.src, e.span) {
                    let num = number(self.src, at.clone(), *value);
                    self.push(group, label, e.span, at.start, Kind::Number(num));
                }
            }
            ExprKind::Neg(inner) => match (&inner.kind, range(self.src, e.span)) {
                // `-4` reads as one number, handle included.
                (ExprKind::Num(v), Some(at)) if self.src.get(at.clone()).is_some_and(|s| s.starts_with('-')) => {
                    let num = number(self.src, at.clone(), -*v);
                    self.push(group, label, e.span, at.start, Kind::Number(num));
                }
                _ => self.expr(inner, group, label),
            },
            ExprKind::Choose(branches) => {
                let options: Vec<Opt> = branches
                    .iter()
                    .filter_map(|(weight, value)| {
                        // A weighted branch (`3: "ember"`) is written in the
                        // code, not here: its span covers only the value.
                        (weight.span == value.span)
                            .then(|| range(self.src, value.span))
                            .flatten()
                            .map(|at| Opt { text: self.src[at.clone()].to_string(), at })
                    })
                    .collect();
                if options.len() == branches.len() && !options.is_empty() {
                    if let Some(start) = range(self.src, e.span).map(|r| r.start) {
                        let body = braces(self.src, start);
                        self.push(group, label, e.span, start, Kind::Choose { options, body });
                        return;
                    }
                }
                for (_, value) in branches {
                    self.expr(value, group, label);
                }
            }
            ExprKind::Draw { dist, per_point } => {
                if let ExprKind::Call(name, args) = &dist.kind {
                    let names = SPREADS.iter().find(|(d, _)| d == name).map(|(_, n)| *n);
                    let both = args.iter().map(|a| literal(self.src, &a.val)).collect::<Option<Vec<_>>>();
                    if let (Some(names), Some(nums), 2) = (names, both, args.len()) {
                        let at = range(self.src, e.span).map_or(nums[0].at.start, |r| r.start);
                        let kind = Kind::Spread {
                            dist: name.clone(),
                            per_point: *per_point,
                            names,
                            from: nums[0].clone(),
                            to: nums[1].clone(),
                        };
                        self.push(group, label, e.span, at, kind);
                        return;
                    }
                }
                self.expr(dist, group, label);
            }
            ExprKind::Call(name, args) => self.args(args, group, name, label),
            ExprKind::Bin(_, a, b) | ExprKind::Index(a, b) => {
                self.expr(a, group, label);
                self.expr(b, group, label);
            }
            ExprKind::Member(a, _) => self.expr(a, group, label),
            ExprKind::List(items) => {
                for item in items {
                    self.expr(item, group, label);
                }
            }
            ExprKind::Comprehension { body, loops } => {
                self.expr(body, group, label);
                for (_, list) in loops {
                    self.expr(list, group, label);
                }
            }
            // A text's insides all carry the text's own span, and a synth is
            // a language of its own: neither can be pointed at safely.
            ExprKind::Var(_) | ExprKind::Str(_) | ExprKind::Template(_) | ExprKind::Synth { .. } => {}
        }
    }
}

/// The number an expression is, if it is one.
fn literal(src: &str, e: &Expr) -> Option<Num> {
    let at = range(src, e.span)?;
    match &e.kind {
        ExprKind::Num(v) => Some(number(src, at, *v)),
        ExprKind::Neg(inner) => match inner.kind {
            ExprKind::Num(v) if src.get(at.clone())?.starts_with('-') => Some(number(src, at, -v)),
            _ => None,
        },
        _ => None,
    }
}

fn number(src: &str, at: Range<usize>, value: f64) -> Num {
    let decimals = src[at.clone()].split_once('.').map_or(0, |(_, d)| d.chars().filter(char::is_ascii_digit).count());
    Num { value, at, decimals }
}

/// The byte range a span covers. Spans count characters from the start of
/// their line, and stay on it.
pub fn range(src: &str, span: Span) -> Option<Range<usize>> {
    if span.line == 0 || span.col == 0 {
        return None;
    }
    let mut start = 0;
    for line in src.split_inclusive('\n').take(span.line - 1) {
        start += line.len();
    }
    let line = src.get(start..)?;
    let from = start + line.char_indices().nth(span.col - 1).map(|(i, _)| i)?;
    let rest = src.get(from..)?;
    let to = from + rest.char_indices().nth(span.len).map_or(rest.len(), |(i, _)| i);
    (to <= src.len() && to > from).then_some(from..to)
}

/// What is inside the braces of the `choose` starting at `from`, when every
/// option of it is on one line.
fn braces(src: &str, from: usize) -> Option<Range<usize>> {
    let open = from + src.get(from..)?.find('{')?;
    let mut depth = 0usize;
    let mut quoted = false;
    let mut comment = false;
    for (i, c) in src.get(open..)?.char_indices() {
        match c {
            '\n' => comment = false,
            _ if comment => {}
            '"' => quoted = !quoted,
            _ if quoted => {}
            '/' | '#' if src[open + i..].starts_with("//") || c == '#' => comment = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + 1..open + i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROSACE: &str = include_str!("../assets/starter.alea");

    fn slice<'a>(src: &'a str, k: &Knob) -> &'a str {
        match &k.kind {
            Kind::Number(n) => &src[n.at.clone()],
            Kind::Spread { from, to, .. } => &src[from.at.start..to.at.end],
            Kind::Choose { options, .. } => &src[options[0].at.start..options.last().unwrap().at.end],
        }
    }

    #[test]
    fn spans_point_at_what_they_name() {
        let src =
            "work W {\n  let a = blob(x: 0.72, size: 0.22)\n  let s = choose { 5 | 6 | 12 }\n  let r = ~between(0.5, 0.8)\n}\n";
        let knobs = knobs(src);
        let found: Vec<_> = knobs.iter().map(|k| (k.group.as_str(), k.label.as_str(), slice(src, k))).collect();
        assert_eq!(found, [("a", "x", "0.72"), ("a", "size", "0.22"), ("s", "s", "5 | 6 | 12"), ("r", "r", "0.5, 0.8"),]);
    }

    #[test]
    fn the_starter_work_has_its_two_choices() {
        let chosen: Vec<_> = knobs(ROSACE)
            .into_iter()
            .filter_map(|k| match k.kind {
                Kind::Choose { options, .. } => Some((k.label, options.into_iter().map(|o| o.text).collect::<Vec<_>>())),
                _ => None,
            })
            .collect();
        assert_eq!(
            chosen,
            [
                ("sides".to_string(), vec!["5".into(), "6".into(), "8".into(), "12".into()]),
                ("palette".to_string(), vec!["\"ember\"".into(), "\"lichen\"".into(), "\"night\"".into()]),
            ]
        );
    }

    #[test]
    fn dragging_moves_a_number_by_about_a_hundredth_of_itself() {
        let number = |text: &str, value: f64| number(text, 0..text.len(), value);
        // A whole number of millimetres moves in millimetres, a ratio
        // written in hundredths in thousandths: forty points of drag is a
        // small change in both.
        assert_eq!(number("80", 80.0).speed(), 0.8);
        assert!((number("0.22", 0.22).speed() - 0.0025).abs() < 1e-9);
        assert!((number("0.015", 0.015).speed() - 0.00025).abs() < 1e-9);
        // Zero still moves.
        assert!(number("0.0", 0.0).speed() > 0.0);
    }

    #[test]
    fn a_number_keeps_its_shape_when_rewritten() {
        let src = "work W {\n  render sculpture(clouds(), side: 80, relief: 0.015)\n}\n";
        let knobs = knobs(src);
        let mut edits = Vec::new();
        for k in &knobs {
            if let Kind::Number(n) = &k.kind {
                edits.push(Edit { at: n.at.clone(), text: n.text(n.value * 2.0) });
            }
        }
        let mut out = src.to_string();
        apply(&mut out, edits);
        assert!(out.contains("side: 160, relief: 0.030"), "{out}");
    }

    #[test]
    fn negative_numbers_keep_their_sign() {
        let src = "work W {\n  let twist = floor(~between(-4, 5))\n}\n";
        let Kind::Spread { from, to, names, per_point, .. } = &knobs(src)[0].kind else { panic!("not a spread") };
        assert_eq!((from.value, to.value, *names, *per_point), (-4.0, 5.0, ["from", "to"], false));
        let mut out = src.to_string();
        apply(&mut out, vec![Edit { at: from.at.clone(), text: from.text(-1.0) }]);
        assert!(out.contains("~between(-1, 5)"), "{out}");
    }

    #[test]
    fn options_are_added_and_removed() {
        let src = "work W {\n  let s = choose { 5 | 6 | 12 }\n}\n";
        let Kind::Choose { options, body } = &knobs(src)[0].kind else { panic!("not a choice") };
        let body = body.clone().expect("a body");
        assert_eq!(&src[body.clone()], " 5 | 6 | 12 ");

        let mut added = src.to_string();
        apply(&mut added, vec![add_option(src, options, &body).unwrap()]);
        assert!(added.contains("choose { 5 | 6 | 12 | 12 }"), "{added}");

        for (k, want) in [(0, "choose { 6 | 12 }"), (1, "choose { 5 | 12 }"), (2, "choose { 5 | 6 }")] {
            let mut out = src.to_string();
            apply(&mut out, vec![remove_option(src, options, k).unwrap()]);
            assert!(out.contains(want), "removing {k} gave {out}");
        }
    }

    #[test]
    fn a_multi_line_choice_grows_a_line() {
        let src = "work W {\n  let w = choose {\n      sin(x)      // ribs\n    | clouds(scale: 5)\n  }\n}\n";
        let Kind::Choose { options, body } = &knobs(src)[0].kind else { panic!("not a choice") };
        let body = body.clone().expect("a body");
        let mut out = src.to_string();
        apply(&mut out, vec![add_option(src, options, &body).unwrap()]);
        assert!(out.contains("| clouds(scale: 5)\n    | clouds(scale: 5)\n"), "{out}");
        // The comment on the first option is still where it was.
        assert!(out.contains("sin(x)      // ribs"), "{out}");
    }

    #[test]
    fn weighted_and_computed_choices_are_left_alone() {
        // A weight is part of the code, and `~clouds()` is not a plain range.
        let src = "work W {\n  let a = choose { 3: \"ember\" | 1: \"night\" }\n  let b = ~normal(mean(a), 1)\n}\n";
        assert!(!knobs(src).iter().any(|k| k.is_draw()), "{:?}", knobs(src));
    }

    #[test]
    fn every_example_of_the_language_is_walked_safely() {
        // Spans must land on character boundaries and inside the source, in
        // every work the engine can parse.
        for src in [
            ROSACE,
            include_str!("../assets/starter.alea"),
            "work É {\n  let é = choose { \"ü\" | \"ñ\" }\n  let n = ~between(0.1, 0.9)\n}\n",
        ] {
            for k in knobs(src) {
                let text = slice(src, &k);
                assert!(!text.is_empty() && src.contains(text), "{text:?} is not in the source");
            }
        }
    }

    #[test]
    fn knobs_are_named_after_what_they_are() {
        // Positional arguments take the names the reference gives them, a
        // lone argument keeps the name of what is being written, and
        // repeats are counted.
        let src = "work W {\n  let turns = floor(~between(6, 15))\n  let p = blob(0.4, 0.6)\n  let o = [~between(0, 1), ~between(2, 3)]\n}\n";
        let named: Vec<_> = knobs(src).into_iter().map(|k| (k.group, k.label)).collect();
        assert_eq!(
            named,
            [
                ("turns".to_string(), "turns".to_string()),
                ("p".to_string(), "x".to_string()),
                ("p".to_string(), "y".to_string()),
                ("o".to_string(), "o 1".to_string()),
                ("o".to_string(), "o 2".to_string()),
            ]
        );
    }

    #[test]
    fn a_work_that_does_not_parse_has_no_knobs() {
        assert!(knobs("work Broken {\n  let a = blob(\n}").is_empty());
    }
}
