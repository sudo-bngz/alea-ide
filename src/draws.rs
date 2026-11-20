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

    /// The same, but never coarser than hundredths: a position dragged
    /// across the canvas needs somewhere to land, even where the work
    /// happened to write a whole number.
    pub fn text_fine(&self, value: f64) -> String {
        format!("{value:.*}", self.decimals.max(2))
    }

    /// What a slider should span for this number: a ratio runs from 0 to 1,
    /// anything else from 0 to twice what it reads, which keeps the handle
    /// in the middle and leaves room to go further. Typing goes past it.
    pub fn track(&self) -> std::ops::RangeInclusive<f64> {
        let v = self.value;
        if v == 0.0 {
            return 0.0..=1.0;
        }
        if self.decimals > 0 && (0.0..=1.0).contains(&v) {
            return 0.0..=1.0;
        }
        if v < 0.0 {
            return (2.0 * v)..=(-2.0 * v);
        }
        let whole = if self.decimals == 0 { 10.0 } else { 1.0 };
        0.0..=(2.0 * v).max(whole)
    }
}

/// One option of a `choose`, as written. A weight is written in the code
/// (`choose { 3: a | 1: b }`); the IDE writes 1 and 0 to hold a choice at
/// one option while keeping the others where they are.
#[derive(Clone, Debug, PartialEq)]
pub struct Opt {
    pub text: String,
    pub at: Range<usize>,
    pub weight: Option<Weight>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weight {
    pub value: f64,
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

/// The call a number was written inside: `blob(x: 0.72, …)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub name: String,
    /// Where the call starts, which tells its numbers apart from another
    /// call's in the same statement.
    pub at: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Knob {
    /// The statement it belongs to: `petal`, `render image`.
    pub group: String,
    /// What it is called where it sits: an argument name, or the binding's.
    /// Repeats within a group are numbered, so this is for reading.
    pub label: String,
    /// The same name before any numbering: what the argument is called in
    /// the call, which is how the canvas knows an `x` from a `size`.
    pub arg: String,
    pub line: usize,
    /// Where the whole thing starts, for ordering and for jumping to it.
    pub at: usize,
    /// The call it was written inside, when it was.
    pub call: Option<Call>,
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

/// Which option a choice is held at: the one with a weight above zero,
/// when every other weight is zero. Weights the artist wrote themselves
/// (two of them above zero) are not a hold.
pub fn held_at(options: &[Opt]) -> Option<usize> {
    let weights: Vec<f64> = options.iter().map(|o| o.weight.as_ref().map_or(f64::NAN, |w| w.value)).collect();
    if weights.iter().any(|w| w.is_nan()) {
        return None;
    }
    let above: Vec<usize> = weights.iter().enumerate().filter(|(_, w)| **w > 0.0).map(|(k, _)| k).collect();
    (above.len() == 1).then(|| above[0])
}

/// Whether the IDE may write weights here: when none are written, or when
/// the ones written are its own.
pub fn can_hold(options: &[Opt]) -> bool {
    options.iter().all(|o| o.weight.is_none()) || held_at(options).is_some()
}

/// Holds a choice at one option, so every edition draws that one. The other
/// options stay in the code at weight zero, ready to come back.
pub fn hold(options: &[Opt], at: usize) -> Vec<Edit> {
    options
        .iter()
        .enumerate()
        .map(|(k, option)| {
            let value = if k == at { "1" } else { "0" };
            match &option.weight {
                Some(weight) => Edit { at: weight.at.clone(), text: value.into() },
                None => Edit { at: option.at.start..option.at.start, text: format!("{value}: ") },
            }
        })
        .collect()
}

/// Lets a held choice go: the weights the hold wrote are taken out again.
pub fn release(options: &[Opt]) -> Vec<Edit> {
    options
        .iter()
        .filter_map(|option| {
            let weight = option.weight.as_ref()?;
            // From the weight to the value takes the colon and its spaces.
            Some(Edit { at: weight.at.start..option.at.start, text: String::new() })
        })
        .collect()
}

/// Removes one option, taking its separator with it.
pub fn remove_option(src: &str, options: &[Opt], at: usize) -> Option<Edit> {
    if options.len() < 2 {
        return None;
    }
    let gone = options.get(at)?;
    let start = gone.weight.as_ref().map_or(gone.at.start, |w| w.at.start);
    let at = match at {
        // The first option takes the bar that follows it.
        0 => start..options[1].weight.as_ref().map_or(options[1].at.start, |w| w.at.start),
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
    let mut walk = Walk { src, out: Vec::new(), call: None };
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
    /// The call whose arguments are being walked.
    call: Option<Call>,
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
            Stmt::Pattern { name, count, attrs, span } => {
                self.expr(count, name, "count");
                self.args(attrs, name, name, name, range(self.src, *span).map(|r| r.start));
            }
            Stmt::Render { kind, args, span } => {
                self.args(args, &format!("render {kind}"), kind, kind, range(self.src, *span).map(|r| r.start))
            }
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
    fn args(&mut self, args: &[Arg], group: &str, call: &str, inherited: &str, at: Option<usize>) {
        let outer = self.call.replace(Call { name: call.to_string(), at: at.unwrap_or(0) }).filter(|_| at.is_some());
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
        self.call = outer;
    }

    fn push(&mut self, group: &str, label: &str, span: Span, at: usize, kind: Kind) {
        let knob = Knob {
            group: group.to_string(),
            label: label.to_string(),
            arg: label.to_string(),
            line: span.line,
            at,
            call: self.call.clone(),
            kind,
        };
        self.out.push(knob);
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
                        let at = range(self.src, value.span)?;
                        // An unweighted branch has the weight the parser
                        // made up for it, which sits on the value itself.
                        let weight = match weight.span == value.span {
                            true => None,
                            // A weight that is not a plain number is the
                            // artist's own arithmetic: leave the whole
                            // choice alone.
                            false => Some(Weight { value: literal(self.src, weight)?.value, at: range(self.src, weight.span)? }),
                        };
                        Some(Opt { text: self.src[at.clone()].to_string(), at, weight })
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
            ExprKind::Call(name, args) => self.args(args, group, name, label, range(self.src, e.span).map(|r| r.start)),
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
    fn the_artists_own_weights_are_not_a_hold() {
        let src = "work W {\n  let a = choose { 3: \"ember\" | 1: \"night\" }\n}\n";
        let Kind::Choose { options, .. } = &knobs(src)[0].kind else { panic!("not a choice") };
        assert_eq!(options.iter().map(|o| o.text.as_str()).collect::<Vec<_>>(), ["\"ember\"", "\"night\""]);
        assert_eq!(held_at(options), None);
        assert!(!can_hold(options), "two weights above zero are the artist's own");
    }

    #[test]
    fn a_draw_on_an_expression_is_left_alone() {
        // `~normal(mean(a), 1)` has no plain numbers to slide.
        let src = "work W {\n  let a = clouds()\n  let b = ~normal(mean(a), 1)\n}\n";
        assert!(!knobs(src).iter().any(|k| matches!(k.kind, Kind::Spread { .. })));
    }

    #[test]
    fn a_choice_is_held_at_one_option_and_let_go_again() {
        let src = "work W {\n  let s = choose { 5 | 6 | 12 }\n}\n";
        let Kind::Choose { options, .. } = &knobs(src)[0].kind else { panic!("not a choice") };
        assert!(can_hold(options) && held_at(options).is_none());

        let mut held = src.to_string();
        apply(&mut held, hold(options, 1));
        assert!(held.contains("choose { 0: 5 | 1: 6 | 0: 12 }"), "{held}");

        // Read back: the choice knows it is held, and at which option.
        let Kind::Choose { options, .. } = &knobs(&held)[0].kind else { panic!("not a choice") };
        assert_eq!(held_at(options), Some(1));
        assert!(can_hold(options), "its own weights can be rewritten");

        let mut moved = held.clone();
        apply(&mut moved, hold(options, 2));
        assert!(moved.contains("choose { 0: 5 | 0: 6 | 1: 12 }"), "{moved}");

        let mut free = held.clone();
        apply(&mut free, release(options));
        assert_eq!(free, src, "letting go leaves the work as it was");
    }

    #[test]
    fn removing_a_held_option_takes_its_weight_with_it() {
        let src = "work W {\n  let s = choose { 0: 5 | 1: 6 | 0: 12 }\n}\n";
        let Kind::Choose { options, .. } = &knobs(src)[0].kind else { panic!("not a choice") };
        for (k, want) in [(0, "choose { 1: 6 | 0: 12 }"), (1, "choose { 0: 5 | 0: 12 }"), (2, "choose { 0: 5 | 1: 6 }")] {
            let mut out = src.to_string();
            apply(&mut out, vec![remove_option(src, options, k).unwrap()]);
            assert!(out.contains(want), "removing {k} gave {out}");
        }
    }

    #[test]
    fn a_slider_spans_something_sensible() {
        let number = |text: &str, value: f64| number(text, 0..text.len(), value);
        assert_eq!(number("0.22", 0.22).track(), 0.0..=1.0);
        assert_eq!(number("80", 80.0).track(), 0.0..=160.0);
        assert_eq!(number("5", 5.0).track(), 0.0..=10.0);
        assert_eq!(number("-4", -4.0).track(), -8.0..=8.0);
        assert_eq!(number("1.2", 1.2).track(), 0.0..=2.4);
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
    fn a_number_knows_the_call_it_was_written_in() {
        let src = "work W {\n  let a = merge(blob(x: 0.7, y: 0.5, size: 0.2), circle(x: 0.3, y: 0.4))\n}\n";
        let knobs = knobs(src);
        let called: Vec<_> = knobs.iter().map(|k| (k.call.as_ref().map(|c| c.name.as_str()), k.arg.as_str())).collect();
        assert_eq!(
            called,
            [(Some("blob"), "x"), (Some("blob"), "y"), (Some("blob"), "size"), (Some("circle"), "x"), (Some("circle"), "y"),]
        );
        // The two calls are told apart, so the x of one is never read as
        // the x of the other.
        let at: Vec<usize> = knobs.iter().filter_map(|k| k.call.as_ref().map(|c| c.at)).collect();
        assert_eq!(at[0], at[2], "the same blob");
        assert_ne!(at[0], at[3], "a different call");
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
