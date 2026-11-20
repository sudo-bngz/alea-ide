//! Sending a work's music out to instruments.
//!
//! alea already writes each layer to its own MIDI channel; here each layer
//! can also be given a device of its own, the way a track in a DAW is
//! pointed at an output. The messages are the ones `render midi` writes, so
//! what the instruments play is what the file would hold.
//!
//! The sending happens on a thread of its own: MIDI wants its moments kept,
//! and the interface stops for nobody.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use alea::midi::Score;
use midir::{MidiOutput, MidiOutputConnection};

/// Where a layer's messages go.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum To {
    /// Nowhere: the layer is not sent out.
    #[default]
    Nowhere,
    /// A port of alea's own, which other programs can read from.
    Ours,
    /// Not an instrument at all: the messages, written out as words on the
    /// terminal, the way `alea --midi-port print` writes them. For seeing
    /// what a lane sends when there is nothing to send it to.
    Text,
    /// An instrument, or another program, already on the machine.
    Port(String),
}

impl To {
    /// The name the port is opened under.
    fn name(&self) -> Option<&str> {
        match self {
            To::Nowhere | To::Text => None,
            To::Ours => Some(OURS),
            To::Port(name) => Some(name),
        }
    }

    pub fn shown(&self) -> String {
        match self {
            To::Nowhere => "nowhere".into(),
            To::Text => "as words, on the terminal".into(),
            To::Ours => format!("{OURS} (a port of our own)"),
            To::Port(name) => name.clone(),
        }
    }
}

/// The port alea-ide offers to other programs: open it in a DAW and the
/// lanes pointed here play into it.
pub const OURS: &str = "alea-ide";

/// Where one layer plays, and on which channel. The channel alea wrote is
/// kept unless something else is asked for, since a work gives each layer
/// its own already.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Route {
    pub to: To,
    /// 1 to 16, or none for the channel the work wrote.
    pub channel: Option<u8>,
}

impl Route {
    pub fn sends(&self) -> bool {
        self.to != To::Nowhere
    }
}

/// The output ports on the machine right now.
pub fn ports() -> Vec<String> {
    let Ok(out) = MidiOutput::new("alea-ide") else { return Vec::new() };
    out.ports().iter().filter_map(|p| out.port_name(p).ok()).filter(|n| n != OURS).collect()
}

/// A message to send, once its routing has been applied.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Out {
    to: To,
    bytes: Vec<u8>,
}

/// The messages due in `[from, to)`, each with the device it belongs to and
/// the channel it is to be played on. A moment belongs to the window that
/// opens on it, so the note written at the top of the loop is played there.
fn due(score: &Score, routes: &[Route], from: f64, to: f64) -> Vec<Out> {
    let mut out = Vec::new();
    for msg in &score.msgs {
        if msg.at < from || msg.at >= to {
            continue;
        }
        let Some(route) = routes.get(msg.track) else { continue };
        if !route.sends() {
            continue;
        }
        let mut bytes = msg.bytes.clone();
        // A channel of its own, if one was asked for. Anything that is not
        // a channel message (clock, reset) is left as it is.
        if let (Some(channel), Some(status)) = (route.channel, bytes.first().copied()) {
            if (0x80..0xF0).contains(&status) {
                bytes[0] = (status & 0xF0) | (channel.clamp(1, 16) - 1);
            }
        }
        out.push(Out { to: route.to.clone(), bytes });
    }
    out
}

/// What is sounding, so that it can be silenced wherever it is sounding —
/// and every channel that has been played on at all, since an instrument
/// can be holding a note this never sent.
#[derive(Default)]
struct Sounding {
    notes: BTreeSet<(To, u8, u8)>,
    channels: BTreeSet<(To, u8)>,
}

impl Sounding {
    fn watch(&mut self, out: &Out) {
        let Some(status) = out.bytes.first().copied() else { return };
        if status >= 0xF0 {
            return;
        }
        let channel = status & 0x0F;
        self.channels.insert((out.to.clone(), channel));
        match (status & 0xF0, out.bytes.len()) {
            (0x90, 3) if out.bytes[2] > 0 => {
                self.notes.insert((out.to.clone(), channel, out.bytes[1]));
            }
            (0x80 | 0x90, 3) => {
                self.notes.remove(&(out.to.clone(), channel, out.bytes[1]));
            }
            _ => {}
        }
    }

    /// Note off for everything still held, then all-notes-off on every
    /// channel that has been played on: silence, whatever was missed.
    fn silence(&mut self) -> Vec<Out> {
        let held = std::mem::take(&mut self.notes);
        let mut out: Vec<Out> =
            held.iter().map(|(to, ch, note)| Out { to: to.clone(), bytes: vec![0x80 | ch, *note, 0] }).collect();
        let touched = std::mem::take(&mut self.channels);
        out.extend(touched.into_iter().map(|(to, ch)| Out { to, bytes: vec![0xB0 | ch, 123, 0] }));
        out
    }
}

enum Word {
    /// Play this, from the top.
    Play(Arc<Score>, Vec<Route>),
    /// The same music, routed differently, without stopping.
    Route(Vec<Route>),
    /// Another edition of the same work, which takes over where this one is
    /// — a step moved, a knob turned, and round it comes changed.
    Swap(Arc<Score>),
    Stop,
    Quit,
}

/// The way out: a thread that keeps the moments, and the ports it has open.
pub struct Desk {
    say: mpsc::Sender<Word>,
    playing: Arc<AtomicBool>,
    /// Where it is up to, in milliseconds.
    at: Arc<AtomicU64>,
    trouble: Arc<Mutex<Option<String>>>,
}

impl Default for Desk {
    fn default() -> Self {
        Desk::new()
    }
}

impl Desk {
    pub fn new() -> Desk {
        let (say, hear) = mpsc::channel();
        let playing = Arc::new(AtomicBool::new(false));
        let at = Arc::new(AtomicU64::new(0));
        let trouble = Arc::new(Mutex::new(None));
        let desk = Desk { say, playing: Arc::clone(&playing), at: Arc::clone(&at), trouble: Arc::clone(&trouble) };
        std::thread::Builder::new()
            .name("midi".into())
            .spawn(move || sending(&hear, &playing, &at, &trouble))
            .expect("a thread to send MIDI on");
        desk
    }

    pub fn play(&self, score: Arc<Score>, routes: Vec<Route>) {
        let _ = self.say.send(Word::Play(score, routes));
    }

    /// Changes where the lanes go while it plays, as a DAW does.
    pub fn route(&self, routes: Vec<Route>) {
        let _ = self.say.send(Word::Route(routes));
    }

    /// Puts another edition in its place without stopping the clock.
    pub fn swap(&self, score: Arc<Score>) {
        let _ = self.say.send(Word::Swap(score));
    }

    pub fn stop(&self) {
        let _ = self.say.send(Word::Stop);
    }

    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Where it is up to, in seconds.
    pub fn at(&self) -> f64 {
        self.at.load(Ordering::Relaxed) as f64 / 1000.0
    }

    pub fn trouble(&self) -> Option<String> {
        self.trouble.lock().ok().and_then(|t| t.clone())
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        let _ = self.say.send(Word::Quit);
    }
}

/// The ports that are open, opened as they are first needed.
#[derive(Default)]
struct Desks(BTreeMap<To, MidiOutputConnection>);

impl Desks {
    fn send(&mut self, out: &Out, now: f64) -> Result<(), String> {
        if out.to == To::Text {
            println!("{now:8.3}  {}", alea::live::describe(&out.bytes));
            return Ok(());
        }
        if !self.0.contains_key(&out.to) {
            self.0.insert(out.to.clone(), open(&out.to)?);
        }
        let port = self.0.get_mut(&out.to).ok_or("the port closed")?;
        port.send(&out.bytes).map_err(|e| format!("{}: {e}", out.to.shown()))
    }

    /// Lets go of every port but the ones still wanted, so an instrument
    /// taken off a lane is not held open.
    fn keep(&mut self, wanted: &[Route]) {
        self.0.retain(|to, _| wanted.iter().any(|r| r.to == *to));
    }
}

/// Opens a port: one on the machine, by name, or one of our own that other
/// programs can read from.
fn open(to: &To) -> Result<MidiOutputConnection, String> {
    let out = MidiOutput::new("alea-ide").map_err(|e| e.to_string())?;
    let name = to.name().ok_or("nowhere to send")?;
    if *to == To::Ours {
        #[cfg(unix)]
        {
            use midir::os::unix::VirtualOutput;
            return out.create_virtual(OURS).map_err(|e| format!("could not offer a port of our own: {e}"));
        }
        #[cfg(not(unix))]
        return Err("this system has no ports of its own to offer: choose one of the machine's".into());
    }
    let port = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).is_ok_and(|n| n == name))
        .ok_or_else(|| format!("{name} is not there any more"))?;
    out.connect(&port, "alea-ide").map_err(|e| format!("{name} would not open: {e}"))
}

/// The thread: waits for something to play, then keeps its moments.
fn sending(hear: &mpsc::Receiver<Word>, playing: &AtomicBool, at: &AtomicU64, trouble: &Mutex<Option<String>>) {
    let (mut desks, mut sounding) = (Desks::default(), Sounding::default());
    let mut score: Option<(Arc<Score>, Vec<Route>)> = None;
    let mut began = Instant::now();
    let mut last = 0.0_f64;
    let hush = |desks: &mut Desks, sounding: &mut Sounding| {
        for out in sounding.silence() {
            let _ = desks.send(&out, 0.0);
        }
    };
    loop {
        // While it plays, look in often enough to keep the moments; when it
        // does not, wait for something to do.
        let word = match playing.load(Ordering::Relaxed) {
            true => hear.try_recv().map_err(|e| e == mpsc::TryRecvError::Disconnected),
            false => hear.recv().map_err(|_| true),
        };
        match word {
            Ok(Word::Play(new, routes)) => {
                hush(&mut desks, &mut sounding);
                desks.keep(&routes);
                score = Some((new, routes));
                began = Instant::now();
                last = 0.0;
                at.store(0, Ordering::Relaxed);
                playing.store(true, Ordering::Relaxed);
            }
            Ok(Word::Swap(new)) => {
                if let Some((old, _)) = &mut score {
                    hush(&mut desks, &mut sounding);
                    *old = new;
                }
            }
            Ok(Word::Route(routes)) => {
                if let Some((_, old)) = &mut score {
                    // Silence what the lanes that moved were playing.
                    hush(&mut desks, &mut sounding);
                    *old = routes;
                }
            }
            Ok(Word::Stop) => {
                playing.store(false, Ordering::Relaxed);
                hush(&mut desks, &mut sounding);
                desks.0.clear();
            }
            Ok(Word::Quit) | Err(true) => {
                hush(&mut desks, &mut sounding);
                return;
            }
            Err(false) => {}
        }
        let Some((score, routes)) = &score else { continue };
        if !playing.load(Ordering::Relaxed) {
            continue;
        }
        let now = began.elapsed().as_secs_f64();
        at.store((now * 1000.0) as u64, Ordering::Relaxed);
        let mut said = None;
        for out in due(score, routes, last, now) {
            sounding.watch(&out);
            if let Err(why) = desks.send(&out, now) {
                said = Some(why);
            }
        }
        if let Ok(mut t) = trouble.lock() {
            *t = said.or_else(|| t.clone());
        }
        last = now;
        // The music is a loop: round it goes, with nothing left hanging.
        if now >= score.duration {
            hush(&mut desks, &mut sounding);
            began = Instant::now();
            last = 0.0;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alea::midi::Msg;

    fn score() -> Score {
        Score {
            tracks: vec!["bass".into(), "hats".into()],
            tempo: 120.0,
            duration: 4.0,
            msgs: vec![
                Msg { at: 0.0, track: 0, bytes: vec![0x90, 40, 100] },
                Msg { at: 0.5, track: 1, bytes: vec![0x91, 42, 80] },
                Msg { at: 1.0, track: 0, bytes: vec![0x80, 40, 0] },
            ],
        }
    }

    fn to_port(name: &str) -> Route {
        Route { to: To::Port(name.into()), channel: None }
    }

    #[test]
    fn a_lane_that_goes_nowhere_sends_nothing() {
        let routes = vec![Route::default(), to_port("Sub 37")];
        let out = due(&score(), &routes, 0.0, 4.0);
        assert_eq!(out.len(), 1, "only the hats are routed");
        assert_eq!(out[0].to, To::Port("Sub 37".into()));
    }

    #[test]
    fn only_what_falls_in_the_moment_is_sent() {
        let routes = vec![to_port("a"), to_port("b")];
        let now = due(&score(), &routes, 0.4, 0.6);
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].bytes, vec![0x91, 42, 80]);
        // The edge belongs to the window it opens, not the one it closes.
        assert_eq!(due(&score(), &routes, 0.5, 0.6).len(), 1);
        assert!(due(&score(), &routes, 0.4, 0.5).is_empty());
        // And the first note of all is played, not skipped.
        assert_eq!(due(&score(), &routes, 0.0, 0.1).len(), 1);
    }

    #[test]
    fn a_lane_can_be_given_a_channel_of_its_own() {
        let routes = vec![Route { to: To::Port("Sub 37".into()), channel: Some(5) }, Route::default()];
        let out = due(&score(), &routes, 0.0, 4.0);
        assert_eq!(out[0].bytes, vec![0x94, 40, 100], "note on, channel 5");
        assert_eq!(out[1].bytes, vec![0x84, 40, 0], "and its note off follows it");
    }

    #[test]
    fn the_channel_the_work_wrote_is_kept_by_default() {
        let routes = vec![to_port("a"), to_port("b")];
        let out = due(&score(), &routes, 0.0, 1.0);
        assert_eq!(out[1].bytes[0], 0x91, "the hats keep the work's channel 2");
    }

    #[test]
    fn everything_sounding_is_silenced_wherever_it_sounds() {
        let mut sounding = Sounding::default();
        let on = |to: To, ch: u8, note: u8| Out { to, bytes: vec![0x90 | ch, note, 90] };
        sounding.watch(&on(To::Port("a".into()), 0, 40));
        sounding.watch(&on(To::Port("b".into()), 1, 42));
        // A note let go of is not held twice.
        sounding.watch(&Out { to: To::Port("a".into()), bytes: vec![0x80, 40, 0] });
        sounding.watch(&on(To::Port("a".into()), 0, 44));
        let hush = sounding.silence();
        assert_eq!(hush.iter().filter(|o| o.bytes[0] & 0xF0 == 0x80).count(), 2, "{hush:?}");
        // All notes off on each channel that was played on, once — even one
        // with nothing held, in case an instrument is holding it.
        let all: Vec<&Out> = hush.iter().filter(|o| o.bytes[1] == 123).collect();
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(sounding.silence().is_empty(), "silenced twice over");
    }

    #[test]
    fn a_note_off_at_the_very_end_still_goes_out() {
        let routes = vec![to_port("a"), to_port("b")];
        let last = due(&score(), &routes, 0.9, 4.1);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].bytes, vec![0x80, 40, 0]);
    }

    #[test]
    fn asking_the_machine_for_its_ports_is_safe_anywhere() {
        // There may be none here, and that is an answer, not a crash.
        let _ = ports();
    }
}
