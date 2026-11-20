//! What is open: documents (the text of a work) and tabs (a work at a seed).
//!
//! A tab is a document plus a seed, so the same work can sit in two tabs as
//! two editions. Tabs on the same document share its text.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::TextureHandle;

use crate::engine::{Failure, Summary};

pub const STARTER: &str = include_str!("../assets/starter.alea");

/// Works that come with the IDE, to open and pull apart. The first one is
/// what a new window starts on.
pub const EXAMPLES: &[(&str, &str)] = &[("rosace", STARTER), ("breathing", include_str!("../assets/breathing.alea"))];

pub struct Doc {
    pub path: Option<PathBuf>,
    pub text: String,
    /// What to call a work that has no file yet.
    unsaved_name: String,
    /// Bumped on every edit; previews and thumbnails remember which one they show.
    pub version: u64,
    saved: u64,
    pub changed_at: Instant,
}

impl Doc {
    pub fn new(path: Option<PathBuf>, text: String) -> Self {
        Doc { path, text, unsaved_name: "untitled.alea".into(), version: 0, saved: 0, changed_at: Instant::now() }
    }

    /// One of the works that come with the IDE, under its own name so the
    /// errors and the command line it shows read like the real thing.
    pub fn example(name: &str) -> Option<Self> {
        let (name, text) = EXAMPLES.iter().find(|(n, _)| *n == name)?;
        let mut doc = Doc::new(None, (*text).to_string());
        doc.unsaved_name = format!("{name}.alea");
        Some(doc)
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(Doc::new(Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())), text))
    }

    pub fn file_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map_or_else(|| self.unsaved_name.clone(), |n| n.to_string_lossy().into_owned())
    }

    /// The folder `use` paths are relative to, and where exports go by default.
    pub fn dir(&self) -> PathBuf {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .filter(|d| !d.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    }

    pub fn edited(&mut self) {
        self.version += 1;
        self.changed_at = Instant::now();
    }

    pub fn dirty(&self) -> bool {
        self.version != self.saved
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Err(std::io::Error::other("this work has no file yet"));
        };
        std::fs::write(path, &self.text)?;
        self.saved = self.version;
        Ok(())
    }
}

/// A loop at preview size, filling up frame by frame as the engine draws
/// them.
pub struct Clip {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    /// The rate the work asks for, which an export starts from.
    pub asked_fps: f64,
    pub duration: f64,
    pub frames: Vec<Option<TextureHandle>>,
    pub ready: usize,
}

impl Clip {
    pub fn new(width: usize, height: usize, fps: f64, asked_fps: f64, duration: f64, frames: usize) -> Self {
        Clip { width, height, fps, asked_fps, duration, frames: (0..frames).map(|_| None).collect(), ready: 0 }
    }

    pub fn put(&mut self, at: usize, frame: TextureHandle) {
        if let Some(slot) = self.frames.get_mut(at) {
            self.ready += slot.is_none() as usize;
            *slot = Some(frame);
        }
    }

    pub fn done(&self) -> bool {
        self.ready == self.frames.len()
    }

    /// The frame for a moment of the loop, counted from 0 to 1; while the
    /// loop is still being drawn, the last frame before it that is ready.
    pub fn at(&self, position: f64) -> Option<&TextureHandle> {
        let last = self.frames.len().saturating_sub(1);
        let k = ((position.rem_euclid(1.0) * self.frames.len() as f64) as usize).min(last);
        self.frames[..=k].iter().rev().chain(self.frames[k..].iter()).find_map(|f| f.as_ref())
    }
}

/// One edition the tab has been to.
pub struct Shot {
    pub seed: u64,
    pub kept: bool,
    /// A small copy of its preview, and the document version it was drawn from.
    pub thumb: Option<(TextureHandle, u64)>,
}

/// What the tab last asked the engine for: document version, seed, size,
/// and which render.
pub type Ask = (u64, u64, usize, String);

pub struct Tab {
    pub id: u64,
    pub doc: usize,
    pub seed: u64,
    /// The seed field while it is being typed in.
    pub seed_text: String,
    pub history: Vec<Shot>,
    /// Where `seed` is in `history`.
    pub at: usize,
    pub image: Option<TextureHandle>,
    pub image_px: (usize, usize),
    /// Which of the work's renders the viewport shows.
    pub shown: String,
    pub clip: Option<Clip>,
    pub playing: bool,
    /// Where in the loop, from 0 to 1.
    pub position: f64,
    /// Why there is nothing to show, when there is nothing to show.
    pub note: Option<String>,
    pub full: (usize, usize),
    pub summary: Option<Summary>,
    pub failure: Option<Failure>,
    pub elapsed: Option<Duration>,
    pub asked: Option<Ask>,
    /// The serial of the request in flight; replies to older ones are dropped.
    pub serial: u64,
    pub busy: bool,
}

/// Editions to remember per tab before the oldest unkept one goes.
const HISTORY: usize = 48;

impl Tab {
    pub fn new(id: u64, doc: usize, seed: u64) -> Self {
        Tab {
            id,
            doc,
            seed,
            seed_text: format_seed(seed),
            history: vec![Shot { seed, kept: false, thumb: None }],
            at: 0,
            image: None,
            image_px: (0, 0),
            shown: "image".into(),
            clip: None,
            playing: true,
            position: 0.0,
            note: None,
            full: (0, 0),
            summary: None,
            failure: None,
            elapsed: None,
            asked: None,
            serial: 0,
            busy: false,
        }
    }

    /// Shows something the work actually renders: a work with no `render
    /// image` opens on whatever it does render.
    pub fn follow(&mut self) {
        let Some(summary) = &self.summary else { return };
        if summary.renders.contains(&self.shown) {
            return;
        }
        if let Some(first) = summary.renders.first() {
            self.shown = first.clone();
            self.position = 0.0;
        }
    }

    /// Moves to an edition that is already in the history.
    pub fn revisit(&mut self, at: usize) {
        if let Some(shot) = self.history.get(at) {
            self.at = at;
            self.seed = shot.seed;
            self.seed_text = format_seed(self.seed);
        }
    }

    /// Moves to `seed`, adding it to the history if it is new.
    pub fn go(&mut self, seed: u64) {
        if let Some(at) = self.history.iter().position(|s| s.seed == seed) {
            return self.revisit(at);
        }
        if self.history.len() >= HISTORY {
            if let Some(old) = self.history.iter().position(|s| !s.kept) {
                self.history.remove(old);
            }
        }
        self.history.push(Shot { seed, kept: false, thumb: None });
        self.revisit(self.history.len() - 1);
    }

    pub fn reroll(&mut self) {
        self.go(random_seed());
    }

    /// One edition back (`-1`) or forward (`1`) in the history.
    pub fn step(&mut self, by: isize) {
        let at = self.at as isize + by;
        if at >= 0 && (at as usize) < self.history.len() {
            self.revisit(at as usize);
        }
    }
}

/// Seeds are drawn short enough to read out and type back.
pub fn random_seed() -> u64 {
    rand::random::<u32>() as u64
}

pub fn format_seed(seed: u64) -> String {
    format!("0x{seed:x}")
}

/// A seed as the command line takes it: decimal, or hexadecimal after `0x`.
pub fn parse_seed(s: &str) -> Option<u64> {
    let s = s.trim().replace('_', "");
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_works_that_come_with_the_ide_are_named_after_themselves() {
        let breathing = Doc::example("breathing").expect("breathing");
        assert_eq!(breathing.file_name(), "breathing.alea");
        assert!(breathing.text.contains("render video"), "it is the one that moves");
        assert_eq!(Doc::example("rosace").map(|d| d.file_name()).as_deref(), Some("rosace.alea"));
        assert!(Doc::example("nothing").is_none());
        // A saved file keeps its own name.
        assert_eq!(Doc::new(None, String::new()).file_name(), "untitled.alea");
    }

    #[test]
    fn seeds_round_trip() {
        assert_eq!(parse_seed("0x5"), Some(5));
        assert_eq!(parse_seed(" 42 "), Some(42));
        assert_eq!(parse_seed("0xFFFF_FFFF"), Some(0xffff_ffff));
        assert_eq!(parse_seed("ember"), None);
        assert_eq!(parse_seed(&format_seed(0x2f1)), Some(0x2f1));
    }

    #[test]
    fn history_remembers_and_revisits() {
        let mut tab = Tab::new(1, 0, 5);
        tab.go(7);
        tab.go(9);
        assert_eq!((tab.seed, tab.at, tab.history.len()), (9, 2, 3));
        tab.step(-1);
        assert_eq!((tab.seed, tab.seed_text.as_str()), (7, "0x7"));
        tab.step(-5);
        assert_eq!(tab.seed, 7, "stepping past the start stays put");
        tab.go(5);
        assert_eq!((tab.at, tab.history.len()), (0, 3), "a known seed is revisited, not added twice");
    }

    #[test]
    fn history_keeps_the_kept() {
        let mut tab = Tab::new(1, 0, 0);
        tab.history[0].kept = true;
        for seed in 1..200 {
            tab.go(seed);
        }
        assert!(tab.history.len() <= HISTORY);
        assert!(tab.history.iter().any(|s| s.seed == 0 && s.kept));
        assert_eq!(tab.seed, 199);
    }
}
