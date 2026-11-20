//! What is open: documents (the text of a work) and tabs (a work at a seed).
//!
//! A tab is a document plus a seed, so the same work can sit in two tabs as
//! two editions. Tabs on the same document share its text.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::TextureHandle;

use crate::engine::{Failure, Summary};

pub const STARTER: &str = include_str!("../assets/starter.alea");

pub struct Doc {
    pub path: Option<PathBuf>,
    pub text: String,
    /// Bumped on every edit; previews and thumbnails remember which one they show.
    pub version: u64,
    saved: u64,
    pub changed_at: Instant,
}

impl Doc {
    pub fn new(path: Option<PathBuf>, text: String) -> Self {
        Doc { path, text, version: 0, saved: 0, changed_at: Instant::now() }
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(Doc::new(Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())), text))
    }

    pub fn file_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map_or_else(|| "untitled.alea".into(), |n| n.to_string_lossy().into_owned())
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

/// One edition the tab has been to.
pub struct Shot {
    pub seed: u64,
    pub kept: bool,
    /// A small copy of its preview, and the document version it was drawn from.
    pub thumb: Option<(TextureHandle, u64)>,
}

/// What the tab last asked the engine for: document version, seed, size.
pub type Ask = (u64, u64, usize);

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
            full: (0, 0),
            summary: None,
            failure: None,
            elapsed: None,
            asked: None,
            serial: 0,
            busy: false,
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
