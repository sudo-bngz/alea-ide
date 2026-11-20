//! The music of an edition, as something to look at.
//!
//! alea hands out a work's music as MIDI messages in time order — the same
//! ones it would send to a DAW. This reads them back into lanes of notes
//! and controller curves, which is what a roll is made of.

use std::collections::HashMap;

use alea::midi::Score;

/// One note, from when it starts to when it lets go.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub at: f64,
    pub until: f64,
    pub pitch: u8,
    pub velocity: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lane {
    /// What a layer plays.
    Notes {
        notes: Vec<Note>,
        /// The lowest and highest pitch in the lane, so it can be drawn
        /// against its own range rather than the whole keyboard.
        low: u8,
        high: u8,
    },
    /// A knob being turned: `cc(lfo(4), 74)`.
    Turns { controller: u8, points: Vec<(f64, f64)> },
    /// A layer that is there but plays nothing in this edition — a drone
    /// whose field never rises enough to sound, say.
    Silent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Music {
    pub tempo: f64,
    pub duration: f64,
    pub lanes: Vec<(String, Lane)>,
}

impl Music {
    /// How many beats the music lasts.
    pub fn beats(&self) -> f64 {
        self.duration * self.tempo / 60.0
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }
}

/// Reads a score into lanes. Everything that is not a note or a controller
/// — the programs and the bends alea sends to set an instrument up — is
/// left out: a roll shows what is played, not how it is dressed.
pub fn read(score: &Score) -> Music {
    // Gathered side by side: a layer can have notes and turn a knob at the
    // same time, as a drone does when it follows its own loudness.
    let mut played: Vec<Vec<Note>> = vec![Vec::new(); score.tracks.len()];
    let mut turned: Vec<HashMap<u8, Vec<(f64, f64)>>> = vec![HashMap::new(); score.tracks.len()];
    // A note that has started and not yet let go, by track and pitch.
    let mut sounding: Vec<(usize, u8, f64, u8)> = Vec::new();

    for msg in &score.msgs {
        let Some((status, data)) = msg.bytes.split_first() else { continue };
        if msg.track >= played.len() {
            continue;
        }
        match (status & 0xF0, data) {
            // A note starting, or a note-on with no weight behind it, which
            // is how MIDI says "stop".
            (0x90, [pitch, velocity]) if *velocity > 0 => sounding.push((msg.track, *pitch, msg.at, *velocity)),
            (0x80, [pitch, _]) | (0x90, [pitch, _]) => {
                let Some(k) = sounding.iter().rposition(|(t, p, ..)| *t == msg.track && p == pitch) else { continue };
                let (_, pitch, from, velocity) = sounding.remove(k);
                played[msg.track].push(Note { at: from, until: msg.at.max(from + 0.01), pitch, velocity });
            }
            (0xB0, [controller, value]) => {
                turned[msg.track].entry(*controller).or_default().push((msg.at, *value as f64 / 127.0));
            }
            _ => {}
        }
    }

    // Notes still sounding at the end simply end there.
    for (track, pitch, from, velocity) in sounding {
        played[track].push(Note { at: from, until: score.duration.max(from + 0.01), pitch, velocity });
    }

    let mut lanes: Vec<(String, Lane)> = Vec::new();
    for (track, name) in score.tracks.iter().enumerate() {
        let mut notes = std::mem::take(&mut played[track]);
        if !notes.is_empty() {
            notes.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.pitch.cmp(&b.pitch)));
            let low = notes.iter().map(|n| n.pitch).min().unwrap_or(0);
            let high = notes.iter().map(|n| n.pitch).max().unwrap_or(127);
            lanes.push((name.clone(), Lane::Notes { notes, low, high }));
            continue;
        }
        // No notes: a lane of its own for a knob being turned, which is
        // what `cc(field, number)` is. One point is not a curve.
        let most = turned[track].iter().max_by_key(|(_, points)| points.len());
        match most.filter(|(_, points)| points.len() > 1) {
            Some((controller, points)) => {
                lanes.push((name.clone(), Lane::Turns { controller: *controller, points: points.clone() }))
            }
            None => lanes.push((name.clone(), Lane::Silent)),
        }
    }
    name_the_drums(&mut lanes);
    name_the_knobs(&mut lanes);
    number_the_repeats(&mut lanes);
    Music { tempo: score.tempo, duration: score.duration, lanes }
}

/// The piece of the kit a drum note strikes. alea lays its kit out as
/// General MIDI does, so the number says which drum it is.
fn drum(pitch: u8) -> Option<&'static str> {
    Some(match pitch {
        36 => "kick",
        38 => "snare",
        39 => "clap",
        41 => "low tom",
        45 => "mid tom",
        48 => "high tom",
        42 => "closed hat",
        46 => "open hat",
        49 => "crash",
        51 => "ride",
        37 | 44 | 75 | 76 | 77 => "tick",
        _ => return None,
    })
}

/// Three lanes all called `drum` say nothing; `kick`, `snare` and `hats`
/// do. A drum lane is named after the piece it strikes most.
fn name_the_drums(lanes: &mut [(String, Lane)]) {
    for (name, lane) in lanes.iter_mut() {
        if !name.ends_with("drum") && !name.ends_with("tick") {
            continue;
        }
        let Lane::Notes { notes, .. } = lane else { continue };
        let mut counts: HashMap<u8, usize> = HashMap::new();
        for note in notes.iter() {
            *counts.entry(note.pitch).or_default() += 1;
        }
        let most = counts.into_iter().max_by_key(|(pitch, count)| (*count, std::cmp::Reverse(*pitch)));
        if let Some(piece) = most.and_then(|(pitch, _)| drum(pitch)) {
            *name = piece.to_string();
        }
    }
}

/// What a controller number turns, where MIDI agrees on one. `cc 74` says
/// nothing; `cutoff` says what the curve is doing.
fn knob(controller: u8) -> Option<&'static str> {
    Some(match controller {
        1 => "mod wheel",
        7 => "volume",
        10 => "pan",
        11 => "expression",
        71 => "resonance",
        74 => "cutoff",
        91 => "reverb",
        93 => "chorus",
        _ => return None,
    })
}

/// A knob lane alea named after its number is renamed after what it turns.
/// A lane the work named itself is left alone.
fn name_the_knobs(lanes: &mut [(String, Lane)]) {
    for (name, lane) in lanes.iter_mut() {
        let Lane::Turns { controller, .. } = lane else { continue };
        if !name.trim_start().starts_with("cc ") {
            continue;
        }
        if let Some(turns) = knob(*controller) {
            *name = turns.to_string();
        }
    }
}

/// Two lanes of the same name are told apart by their order.
fn number_the_repeats(lanes: &mut [(String, Lane)]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (name, _) in lanes.iter() {
        *counts.entry(name.clone()).or_default() += 1;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (name, _) in lanes.iter_mut() {
        if counts[name] > 1 {
            let n = seen.entry(name.clone()).or_default();
            *n += 1;
            *name = format!("{name} {n}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alea::midi::Msg;

    fn msg(at: f64, track: usize, bytes: &[u8]) -> Msg {
        Msg { at, track, bytes: bytes.to_vec() }
    }

    #[test]
    fn notes_are_read_from_when_they_start_to_when_they_let_go() {
        let score = Score {
            tracks: vec!["notes - pluck".into()],
            tempo: 120.0,
            duration: 4.0,
            msgs: vec![
                msg(0.0, 0, &[0xC0, 24]),      // the instrument: not a note
                msg(0.0, 0, &[0x90, 60, 100]), // middle C starts
                msg(0.5, 0, &[0x80, 60, 0]),   // and lets go
                msg(1.0, 0, &[0x90, 67, 80]),
                msg(1.5, 0, &[0x90, 67, 0]), // a note-on with no weight: also a stop
            ],
        };
        let music = read(&score);
        assert_eq!(music.lanes.len(), 1);
        let (name, Lane::Notes { notes, low, high }) = &music.lanes[0] else { panic!("not notes") };
        assert_eq!(name, "notes - pluck");
        assert_eq!((*low, *high), (60, 67));
        assert_eq!(notes[0], Note { at: 0.0, until: 0.5, pitch: 60, velocity: 100 });
        assert_eq!(notes[1], Note { at: 1.0, until: 1.5, pitch: 67, velocity: 80 });
        assert_eq!(music.beats(), 8.0);
    }

    #[test]
    fn a_layer_that_turns_a_knob_while_it_plays_is_still_a_layer() {
        // A drone follows its own loudness with expression (controller 11)
        // and plays notes at the same time: the notes are the lane.
        let score = Score {
            tracks: vec!["drone - organ".into()],
            tempo: 120.0,
            duration: 4.0,
            msgs: vec![
                msg(0.0, 0, &[0xB0, 11, 40]),
                msg(0.0, 0, &[0x90, 45, 90]),
                msg(2.0, 0, &[0xB0, 11, 90]),
                msg(3.0, 0, &[0x80, 45, 0]),
            ],
        };
        let music = read(&score);
        let (_, Lane::Notes { notes, .. }) = &music.lanes[0] else { panic!("the drone lost its notes") };
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].until, 3.0);
    }

    #[test]
    fn a_knob_being_turned_is_a_curve_of_its_own() {
        let score = Score {
            tracks: vec!["cc 74".into()],
            tempo: 120.0,
            duration: 2.0,
            msgs: vec![msg(0.0, 0, &[0xB0, 74, 0]), msg(1.0, 0, &[0xB0, 74, 127])],
        };
        let music = read(&score);
        let (name, Lane::Turns { controller, points }) = &music.lanes[0] else { panic!("not a curve") };
        assert_eq!(*controller, 74);
        assert_eq!(points, &[(0.0, 0.0), (1.0, 1.0)]);
        // And it is named after what the knob does, not after its number.
        assert_eq!(name, "cutoff");
    }

    #[test]
    fn a_knob_nobody_agrees_on_keeps_its_number() {
        let score = Score {
            tracks: vec!["cc 20".into(), "wobble".into()],
            tempo: 120.0,
            duration: 2.0,
            msgs: vec![
                msg(0.0, 0, &[0xB0, 20, 0]),
                msg(1.0, 0, &[0xB0, 20, 127]),
                // A lane the work named itself keeps its own name.
                msg(0.0, 1, &[0xB0, 74, 10]),
                msg(1.0, 1, &[0xB0, 74, 110]),
            ],
        };
        let music = read(&score);
        let names: Vec<&str> = music.lanes.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["cc 20", "wobble"]);
    }

    #[test]
    fn a_layer_that_plays_nothing_says_so() {
        // A drone that never rises enough to sound sets its channel up and
        // then stays quiet: the lane is there, with nothing in it.
        let score = Score {
            tracks: vec!["drone - organ".into()],
            tempo: 120.0,
            duration: 4.0,
            msgs: vec![msg(0.0, 0, &[0xC0, 19]), msg(0.0, 0, &[0xB0, 11, 0])],
        };
        assert_eq!(read(&score).lanes, [("drone - organ".to_string(), Lane::Silent)]);
    }

    #[test]
    fn a_note_still_sounding_at_the_end_ends_there() {
        let score =
            Score { tracks: vec!["drone - organ".into()], tempo: 60.0, duration: 8.0, msgs: vec![msg(1.0, 0, &[0x90, 48, 70])] };
        let (_, Lane::Notes { notes, .. }) = &read(&score).lanes[0] else { panic!("not notes") };
        assert_eq!(notes[0].until, 8.0);
    }

    #[test]
    fn silent_tracks_are_still_lanes() {
        let score = Score {
            tracks: vec!["notes - drum".into(), "drone - organ".into()],
            tempo: 100.0,
            duration: 1.0,
            // The second track is set up but never plays.
            msgs: vec![msg(0.0, 0, &[0x90, 36, 100]), msg(0.2, 0, &[0x80, 36, 0]), msg(0.0, 1, &[0xC0, 19])],
        };
        let music = read(&score);
        assert_eq!(music.lanes.len(), 2, "the silent one is still a lane");
        assert_eq!(music.lanes[0].0, "kick", "a drum lane is named after what it strikes");
        assert_eq!(music.lanes[1].1, Lane::Silent);
    }

    #[test]
    fn a_kit_is_named_after_what_it_strikes() {
        let kit = |track: usize, pitch: u8| vec![msg(0.0, track, &[0x90, pitch, 100]), msg(0.2, track, &[0x80, pitch, 0])];
        let score = Score {
            tracks: vec!["notes - drum".into(), "notes - drum".into(), "notes - pluck".into(), "notes - pluck".into()],
            tempo: 120.0,
            duration: 1.0,
            msgs: [kit(0, 36), kit(1, 42), kit(2, 60), kit(3, 62)].concat(),
        };
        let music = read(&score);
        let names: Vec<&str> = music.lanes.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["kick", "closed hat", "notes - pluck 1", "notes - pluck 2"]);
    }

    #[test]
    fn the_music_of_a_work_comes_out_whole() {
        // A work with drums, a drone and a knob: what the roll has to show.
        let source = "work Heard {\n  let m = meter(120, bars: 2)\n  let hit = steps(\"X...X...X...X...\", bars: m, y: 0.95)\n  let track = [\n    notes(hit, instrument: \"drum\", tempo: m.tempo),\n    drone(clouds(scale: 3), voices: 2, instrument: \"organ\"),\n    cc(lfo(2), 74)\n  ]\n  render midi(track, duration: m.length, tempo: m.tempo)\n}\n";
        let job = crate::engine::Job {
            id: 1,
            source: std::sync::Arc::from(source),
            file: "heard.alea".into(),
            dir: std::path::PathBuf::from("."),
            seed: 7,
            max_side: 64,
            shown: "midi".into(),
        };
        let ed = crate::engine::edition(&job).unwrap_or_else(|e| panic!("{}", e.msg));
        let score = alea::media::live_score(&ed).expect("music").unwrap_or_else(|e| panic!("{}", e.msg));
        let music = read(&score);

        assert_eq!(music.tempo, 120.0);
        assert!((music.beats() - 8.0).abs() < 0.01, "two bars of four beats");
        let names: Vec<&str> = music.lanes.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"kick"), "the drum lane is named after its piece: {names:?}");
        assert!(names.iter().any(|n| n.starts_with("drone")), "{names:?}");
        let drums = music.lanes.iter().find(|(n, _)| n == "kick").map(|(_, l)| l).unwrap();
        let Lane::Notes { notes, .. } = drums else { panic!("not notes") };
        assert_eq!(notes.len(), 8, "four hits a bar, two bars");
        assert!(music.lanes.iter().any(|(_, l)| matches!(l, Lane::Turns { controller: 74, .. })));
    }
}
