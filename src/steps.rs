//! The patterns a work plays: the `steps("X..x")` strings in the code,
//! lined up with the lanes of the roll.
//!
//! The code stays the truth here too. A step moved in the roll is one
//! character rewritten in the string the work wrote, so a pattern can be
//! tapped out with the mouse or typed, and the two are the same edit.

use std::collections::HashMap;
use std::ops::Range;

use alea::ast::{Expr, ExprKind, Stmt, Work};
use alea::{lexer, parser};

use crate::draws::{self, Edit};

/// How a step is written. The weights are alea's: `X` full, `x` two
/// thirds, `o` a third, `.` nothing.
pub const WEIGHTS: [char; 3] = ['X', 'x', 'o'];
const REST: char = '.';

/// One pattern in the source, and where each of its steps is written.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    /// The pattern as written, inside its quotes.
    pub text: String,
    /// The slice of source it occupies, inside its quotes.
    pub at: Range<usize>,
    /// Where each step sits in `text`. Bar lines and spaces are not steps.
    cells: Vec<usize>,
    /// How many bars the pattern writes. It repeats over the work, so this
    /// says how often it comes round.
    pub bars: usize,
}

impl Pattern {
    pub fn steps(&self) -> usize {
        self.cells.len()
    }

    /// How a step is written now.
    pub fn step(&self, step: usize) -> char {
        self.cells.get(step).and_then(|at| self.text[*at..].chars().next()).unwrap_or(REST)
    }

    pub fn sounds(&self, step: usize) -> bool {
        WEIGHTS.contains(&self.step(step))
    }

    /// Writes one step, as an edit to the work.
    fn write(&self, step: usize, c: char) -> Option<Edit> {
        let at = *self.cells.get(step)?;
        let to = at + self.text[at..].chars().next()?.len_utf8();
        Some(Edit { at: (self.at.start + at)..(self.at.start + to), text: c.to_string() })
    }

    /// On, or off again. A step turned on comes back at the weight it had
    /// before it was silenced, so tapping one off and on again does not
    /// turn a ghost note into an accent.
    pub fn toggle(&self, step: usize) -> Option<Edit> {
        match self.sounds(step) {
            true => self.write(step, REST),
            false => self.write(step, WEIGHTS[0]),
        }
    }

    /// Harder, softer, softer still, then silent: the other way to say it.
    pub fn cycle(&self, step: usize) -> Option<Edit> {
        let now = self.step(step);
        let next = match WEIGHTS.iter().position(|c| *c == now) {
            Some(k) if k + 1 < WEIGHTS.len() => WEIGHTS[k + 1],
            Some(_) => REST,
            None => WEIGHTS[0],
        };
        self.write(step, next)
    }
}

/// What a lane offers the hand.
#[derive(Clone, Debug, PartialEq)]
pub enum Tap {
    /// A pattern written out in the code, step by step.
    Written(Pattern),
    /// A choice between written patterns that the seed is still making.
    /// Held at one option, it becomes a pattern to tap.
    Choosing,
    /// Nothing written to tap: a Euclidean rhythm, a chord progression, a
    /// field, a knob being turned.
    Nothing,
}

impl Tap {
    pub fn pattern(&self) -> Option<&Pattern> {
        match self {
            Tap::Written(p) => Some(p),
            _ => None,
        }
    }
}

/// What each lane of `render <kind>` offers, in the order the lanes come
/// out: one entry a track.
pub fn patterns(src: &str, kind: &str) -> Vec<Tap> {
    let Ok(program) = lexer::lex(src).and_then(parser::parse) else {
        return Vec::new();
    };
    let work = &program.work;
    let named = names(work);
    let Some(layers) = layers(work, kind, &named) else { return Vec::new() };
    layers.iter().map(|layer| tap_of(src, layer, &named, &mut Vec::new())).collect()
}

fn names(work: &Work) -> HashMap<&str, &Expr> {
    let mut named = HashMap::new();
    for stmt in &work.stmts {
        if let Stmt::Let { name, val, .. } | Stmt::Field { name, val, .. } = stmt {
            named.insert(name.as_str(), val);
        }
    }
    named
}

/// The layers a render plays, in order: `render sound(track, …)` where
/// `track` is a list is a layer an entry; a single layer is one lane.
fn layers<'a>(work: &'a Work, kind: &str, named: &HashMap<&'a str, &'a Expr>) -> Option<Vec<&'a Expr>> {
    let played = work.stmts.iter().find_map(|stmt| match stmt {
        Stmt::Render { kind: k, args, .. } if k == kind => args.iter().find(|a| a.name.is_none()).map(|a| &a.val),
        _ => None,
    })?;
    match &follow(played, named, &mut Vec::new())?.kind {
        ExprKind::List(items) => Some(items.iter().collect()),
        _ => Some(vec![played]),
    }
}

/// Follows a name to what it was given, as far as it goes.
fn follow<'a>(e: &'a Expr, named: &HashMap<&'a str, &'a Expr>, seen: &mut Vec<&'a str>) -> Option<&'a Expr> {
    match &e.kind {
        ExprKind::Var(name) => {
            if seen.contains(&name.as_str()) {
                return None;
            }
            seen.push(name);
            follow(named.get(name.as_str()).copied()?, named, seen)
        }
        _ => Some(e),
    }
}

/// The written pattern a layer plays, if it plays one. A layer is
/// `notes(rhythm, …)`; the rhythm is a `steps("…")`, possibly by way of a
/// name or of a choice that is being held at one option.
fn tap_of<'a>(src: &str, layer: &'a Expr, named: &HashMap<&'a str, &'a Expr>, seen: &mut Vec<&'a str>) -> Tap {
    let Some(layer) = follow(layer, named, seen) else { return Tap::Nothing };
    let ExprKind::Call(name, args) = &layer.kind else { return Tap::Nothing };
    let mut plain = args.iter().filter(|a| a.name.is_none()).map(|a| &a.val);
    let rhythm = match name.as_str() {
        // The pattern itself, written where the layer is played.
        "steps" => return written(src, layer).map_or(Tap::Nothing, Tap::Written),
        // `notes(rhythm, …)`, but `arp(harmony, rhythm, …)`.
        "notes" => plain.next(),
        "arp" => plain.nth(1),
        _ => None,
    };
    match rhythm {
        Some(rhythm) => rhythm_of(src, rhythm, named, seen),
        None => Tap::Nothing,
    }
}

fn rhythm_of<'a>(src: &str, rhythm: &'a Expr, named: &HashMap<&'a str, &'a Expr>, seen: &mut Vec<&'a str>) -> Tap {
    let Some(rhythm) = follow(rhythm, named, seen) else { return Tap::Nothing };
    match &rhythm.kind {
        ExprKind::Call(name, _) if name == "steps" => written(src, rhythm).map_or(Tap::Nothing, Tap::Written),
        // A choice the IDE is holding at one option is as good as written;
        // one the seed is still free to make is not, since the work would
        // read differently at the next edition.
        ExprKind::Choose(branches) => {
            let held = draws::range(src, rhythm.span).and_then(|at| {
                draws::knobs(src).into_iter().find_map(|knob| match knob.kind {
                    crate::draws::Kind::Choose { options, .. } if knob.at == at.start => draws::held_at(&options),
                    _ => None,
                })
            });
            match held.and_then(|held| branches.get(held)) {
                Some((_, val)) => rhythm_of(src, val, named, seen),
                // Only worth pointing at if the options are patterns.
                None => match branches.iter().any(|(_, v)| matches!(&v.kind, ExprKind::Call(n, _) if n == "steps")) {
                    true => Tap::Choosing,
                    false => Tap::Nothing,
                },
            }
        }
        _ => Tap::Nothing,
    }
}

/// Reads a `steps("…", bars: n)` call: the string, where it is written, and
/// how many bars it covers.
fn written(src: &str, call: &Expr) -> Option<Pattern> {
    let ExprKind::Call(_, args) = &call.kind else { return None };
    let first = args.iter().find(|a| a.name.is_none())?;
    let ExprKind::Str(text) = &first.val.kind else { return None };
    let at = quoted(src, draws::range(src, first.val.span)?, text)?;
    let cells = text.char_indices().filter(|(_, c)| *c != '|' && !c.is_whitespace()).map(|(i, _)| i).collect::<Vec<_>>();
    if cells.is_empty() {
        return None;
    }
    // `bars:` is how long the work plays the pattern for, not how long the
    // pattern is: that is written in the string, a bar between bar lines.
    let bars = text.chars().filter(|c| *c == '|').count() + 1;
    Some(Pattern { text: text.clone(), at, cells, bars })
}

/// The inside of a string literal: the slice between its quotes, and only
/// when it reads back exactly as the work wrote it.
fn quoted(src: &str, at: Range<usize>, text: &str) -> Option<Range<usize>> {
    let whole = src.get(at.clone())?;
    let open = whole.find(['"', '\''])?;
    let inside = (at.start + open + 1)..(at.start + open + 1 + text.len());
    (src.get(inside.clone()) == Some(text)).then_some(inside)
}

/// Which step of a pattern a moment falls in, and which cell of the roll
/// that is. The pattern repeats over the work, so the work is divided into
/// as many cells as the pattern has steps, as many times as it comes round.
pub fn cells(pattern: &Pattern, bars: f64) -> usize {
    let times = (bars / pattern.bars.max(1) as f64).round().max(1.0);
    pattern.steps() * times as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRUMS: &str = r#"work Drums {
  let m = meter(120, bars: 4)
  let kick = steps("X..x|X.o.", bars: m, y: 0.1)
  let hats = euclid(5, 16, bars: m)
  let kit = [
    notes(kick, instrument: "drum", tempo: m.tempo),
    notes(hats, instrument: "drum", tempo: m.tempo)
  ]
  render sound(kit, duration: m.length)
}
"#;

    #[test]
    fn a_lane_is_lined_up_with_the_pattern_it_plays() {
        let found = patterns(DRUMS, "sound");
        assert_eq!(found.len(), 2, "one entry a lane");
        let kick = found[0].pattern().expect("the kick is written out");
        assert_eq!(kick.text, "X..x|X.o.");
        assert_eq!((kick.steps(), kick.bars), (8, 2));
        assert_eq!(&DRUMS[kick.at.clone()], "X..x|X.o.");
        // A Euclidean rhythm is not written down, so there is nothing to tap.
        assert_eq!(found[1], Tap::Nothing, "euclid is not a written pattern");
    }

    #[test]
    fn tapping_a_step_rewrites_one_character() {
        let kick = patterns(DRUMS, "sound")[0].pattern().unwrap().clone();
        let mut src = DRUMS.to_string();
        // Step 1 is the rest after the first hit: tapping it fills it in.
        draws::apply(&mut src, vec![kick.toggle(1).unwrap()]);
        assert!(src.contains(r#"steps("XX.x|X.o.""#), "{src}");
        // And the other way round.
        let kick = patterns(&src, "sound")[0].pattern().unwrap().clone();
        let mut back = src.clone();
        draws::apply(&mut back, vec![kick.toggle(1).unwrap()]);
        assert!(back.contains(r#"steps("X..x|X.o.""#), "{back}");
    }

    #[test]
    fn a_step_goes_round_the_weights() {
        let kick = patterns(DRUMS, "sound")[0].pattern().unwrap().clone();
        assert_eq!((kick.step(0), kick.step(3), kick.step(6)), ('X', 'x', 'o'));
        let mut src = DRUMS.to_string();
        draws::apply(&mut src, vec![kick.cycle(0).unwrap()]);
        assert!(src.contains(r#"steps("x..x"#), "{src}");
        let soft = patterns(&src, "sound")[0].pattern().unwrap().clone();
        draws::apply(&mut src, vec![soft.cycle(0).unwrap()]);
        assert!(src.contains(r#"steps("o..x"#), "{src}");
        let ghost = patterns(&src, "sound")[0].pattern().unwrap().clone();
        draws::apply(&mut src, vec![ghost.cycle(0).unwrap()]);
        assert!(src.contains(r#"steps("...x"#), "{src}");
    }

    #[test]
    fn bar_lines_and_spaces_are_not_steps() {
        let src = DRUMS.replace(r#""X..x|X.o.""#, r#""X. .x | X.o.""#);
        let kick = patterns(&src, "sound")[0].pattern().unwrap().clone();
        assert_eq!(kick.steps(), 8);
        let mut out = src.clone();
        draws::apply(&mut out, vec![kick.toggle(4).unwrap()]);
        assert!(out.contains(r#""X. .x | ..o.""#), "{out}");
    }

    #[test]
    fn a_choice_the_seed_still_makes_is_left_alone() {
        let src = DRUMS.replace(r#"steps("X..x|X.o.", bars: m, y: 0.1)"#, r#"choose { steps("X..x") | steps("..X.") }"#);
        assert_eq!(patterns(&src, "sound")[0], Tap::Choosing, "a free choice says it could be held");
        // Held at its second option, it is written down after all.
        let held =
            src.replace(r#"choose { steps("X..x") | steps("..X.") }"#, r#"choose { 0: steps("X..x") | 1: steps("..X.") }"#);
        let found = patterns(&held, "sound");
        let kick = found[0].pattern().expect("the held option");
        assert_eq!(kick.text, "..X.");
    }

    #[test]
    fn the_pattern_comes_round_as_often_as_it_fits() {
        let kick = patterns(DRUMS, "sound")[0].pattern().unwrap().clone();
        // Two bars written, four played: eight steps, four times over.
        assert_eq!(cells(&kick, 4.0), 16);
        assert_eq!(cells(&kick, 2.0), 8);
        // Never fewer than the pattern itself.
        assert_eq!(cells(&kick, 1.0), 8);
    }

    #[test]
    fn a_work_with_one_layer_still_has_a_lane() {
        let src = "work One {\n  let m = meter(120, bars: 1)\n  render sound(notes(steps(\"X.x.\", bars: m), instrument: \"drum\", tempo: m.tempo), duration: m.length)\n}\n";
        let found = patterns(src, "sound");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pattern().unwrap().text, "X.x.");
    }

    /// The lanes of the roll are the tracks of the score, in the order the
    /// work lists them, which is the order the patterns come out in here.
    /// If the two ever drift apart, a tap would land in the wrong lane.
    #[test]
    fn the_bundled_example_lines_up_lane_for_lane() {
        let src = include_str!("../assets/bassline.alea");
        let found = patterns(src, "sound");
        let job = crate::engine::Job {
            id: 1,
            source: std::sync::Arc::from(src),
            file: "bassline.alea".into(),
            dir: std::path::PathBuf::from("."),
            seed: 7,
            max_side: 96,
            shown: "sound".into(),
        };
        let ed = crate::engine::edition(&job).map_err(|e| e.msg).expect("bassline reads");
        let score = alea::media::live_score(&ed).transpose().unwrap().expect("bassline plays");
        let music = crate::score::read(&score);
        assert_eq!(found.len(), music.lanes.len(), "a pattern slot for every lane");
        let named: Vec<(&str, Option<&str>)> =
            music.lanes.iter().zip(&found).map(|((n, _), p)| (n.as_str(), p.pattern().map(|p| p.text.as_str()))).collect();
        assert_eq!(named[5], ("ride", Some("X...X...X...X...|X...X...X..oX...")), "{named:?}");
        assert_eq!(named[6], ("open hat", Some("....X.......X...|....X..o....X..o")), "{named:?}");
        assert_eq!(named[7], ("snare", Some("..X...X...X...X.|..X...X...X.o.X.")), "{named:?}");
        // The bass is a choice the seed still makes, the arp is Euclidean,
        // the drone is a field and the knobs are curves: nothing to tap.
        assert!(named[..5].iter().all(|(_, p)| p.is_none()), "{named:?}");
    }

    #[test]
    fn a_work_that_does_not_read_has_no_patterns() {
        assert!(patterns("work Broken {\n  render sound(", "sound").is_empty());
    }
}
