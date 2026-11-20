//! The window: tabs on top, code on the left, the edition in the middle,
//! what it is on the right, and the seed along the bottom.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, ColorImage, Context, FontId, Id, Key, Layout, Modifiers, Pos2, Rect, RichText, Sense, Stroke,
    TextureOptions, Vec2,
};

use crate::complete::{self, Candidate};
use crate::doc::{format_seed, parse_seed, random_seed, Clip, Doc, Tab, STARTER};
use crate::draws::{self, add_option, remove_option, Edit, Kind, Knob, Num, Opt};
use crate::engine::{Engine, Job, Outcome, Preview};
use crate::export::{self, Format, Options, Progress};
use crate::reference::{self, Entry};
use crate::theme;

/// How long the text must rest before a preview is drawn.
const DEBOUNCE: Duration = Duration::from_millis(140);
/// Longest side of a history thumbnail, in pixels.
const THUMB: usize = 144;

type Exported = Result<String, String>;

/// The export dialog: what it is set to, and how far a run has got.
struct Exporting {
    options: Options,
    /// Which editions: this one, or the ones kept in this tab.
    kept: bool,
    running: bool,
    /// What the run is doing, and how far along it is.
    progress: (Sender<Progress>, Receiver<Progress>),
    said: Option<(String, usize, usize, f32)>,
}

pub struct App {
    docs: Vec<Doc>,
    tabs: Vec<Tab>,
    active: usize,
    engine: Engine,
    next_id: u64,
    serial: u64,
    exports: (Sender<Exported>, Receiver<Exported>),
    /// The export dialog, while it is open.
    export: Option<Exporting>,
    /// Where the last export went, offered again for the next one.
    folder: Option<PathBuf>,
    /// A line of news in the top bar, and whether it is bad news.
    notice: Option<(String, bool, Instant)>,
    /// Longest side the viewport can show, in pixels (rounded up, so small
    /// resizes do not redraw).
    side: usize,
    /// The names offered for the word being typed.
    offer: Option<Offer>,
    /// The option of a `choose` being typed into, kept out of the source
    /// until it is finished: half-typed code would parse as nothing and the
    /// controls would vanish under the hand editing them.
    editing: Option<Editing>,
    /// The choice whose options are being edited as text, and the row the
    /// hand is on.
    editing_choice: Option<usize>,
    selected: Option<usize>,
    /// The knobs of the active document, and the version they were read from.
    knobs: Vec<Knob>,
    knobs_of: Option<(usize, u64)>,
    reference: Option<Reference>,
}

/// The completion list, as it stands between two frames.
struct Offer {
    items: Vec<Candidate>,
    at: usize,
    /// The word being completed, in bytes of the document.
    word: std::ops::Range<usize>,
    /// Where to draw the list: under the cursor.
    under: Pos2,
}

struct Editing {
    knob: usize,
    option: usize,
    text: String,
}

/// The reference drawer: what is typed in its search box, and what is open.
#[derive(Default)]
struct Reference {
    query: String,
    shown: Option<String>,
}

enum Key2 {
    None,
    Move(isize),
    Accept,
    Close,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let mut app = App {
            docs: Vec::new(),
            tabs: Vec::new(),
            active: 0,
            engine: Engine::new(move || ctx.request_repaint()),
            next_id: 1,
            serial: 0,
            exports: mpsc::channel(),
            export: None,
            folder: None,
            notice: None,
            side: 512,
            offer: None,
            editing: None,
            editing_choice: None,
            selected: None,
            knobs: Vec::new(),
            knobs_of: None,
            reference: None,
        };
        match file {
            Some(path) => app.open(path),
            None => app.add_tab(Doc::new(None, STARTER.to_string())),
        }
        if app.tabs.is_empty() {
            app.add_tab(Doc::new(None, STARTER.to_string()));
        }
        app
    }

    fn say(&mut self, text: impl Into<String>, bad: bool) {
        self.notice = Some((text.into(), bad, Instant::now()));
    }

    fn add_tab(&mut self, doc: Doc) {
        self.docs.push(doc);
        self.push_tab(self.docs.len() - 1);
    }

    fn push_tab(&mut self, doc: usize) {
        self.tabs.push(Tab::new(self.next_id, doc, random_seed()));
        self.next_id += 1;
        self.active = self.tabs.len() - 1;
    }

    fn open(&mut self, path: PathBuf) {
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        // Already open: a second tab on the same text, at another seed.
        if let Some(doc) = self.docs.iter().position(|d| d.path.as_deref() == Some(canon.as_path())) {
            return self.push_tab(doc);
        }
        match Doc::open(&path) {
            Ok(doc) => self.add_tab(doc),
            Err(e) => self.say(format!("Could not open {}: {e}", path.display()), true),
        }
    }

    fn open_dialog(&mut self) {
        let dir = self.docs[self.tabs[self.active].doc].dir();
        if let Some(path) = rfd::FileDialog::new().add_filter("alea", &["alea"]).set_directory(dir).pick_file() {
            self.open(path);
        }
    }

    fn save(&mut self) {
        let doc = &mut self.docs[self.tabs[self.active].doc];
        if doc.path.is_none() {
            let Some(path) = rfd::FileDialog::new().add_filter("alea", &["alea"]).set_file_name("untitled.alea").save_file()
            else {
                return;
            };
            doc.path = Some(path);
        }
        match doc.save() {
            Ok(()) => {
                let name = doc.file_name();
                self.say(format!("Saved {name}"), false);
            }
            Err(e) => self.say(format!("Could not save: {e}"), true),
        }
    }

    fn close(&mut self, at: usize) {
        if self.tabs.len() == 1 {
            return;
        }
        self.tabs.remove(at);
        if self.active >= at && self.active > 0 {
            self.active -= 1;
        }
    }

    fn job(&self, tab: &Tab, id: u64) -> Job {
        let doc = &self.docs[tab.doc];
        Job {
            id,
            shown: tab.shown.clone(),
            source: Arc::from(doc.text.as_str()),
            file: doc.file_name(),
            dir: doc.dir(),
            seed: tab.seed,
            max_side: self.side,
        }
    }

    /// Opens the export dialog, set to what the work asks for.
    fn export(&mut self) {
        if self.export.is_some() {
            return;
        }
        let tab = &self.tabs[self.active];
        let folder = self.folder.clone().unwrap_or_else(|| self.docs[tab.doc].dir());
        if Format::of(&tab.shown).is_empty() {
            self.say(format!("`{}` cannot be written yet", tab.shown), true);
            return;
        }
        // What the work asks for: the size of the still or of the loop, and
        // the loop's own rate.
        let (width, height) = tab.full;
        let fps = tab.clip.as_ref().map_or(30.0, |c| c.asked_fps);
        let Some(options) = Options::new(&tab.shown, (width, height), fps, folder) else { return };
        self.export = Some(Exporting { options, kept: false, running: false, progress: mpsc::channel(), said: None });
    }

    /// Starts the run the dialog is set to.
    fn export_now(&mut self) {
        let Some(export) = &mut self.export else { return };
        if export.running {
            return;
        }
        let seeds = {
            let tab = &self.tabs[self.active];
            match export.kept {
                true => tab.history.iter().filter(|s| s.kept).map(|s| s.seed).collect(),
                false => vec![tab.seed],
            }
        };
        if seeds.is_empty() {
            return;
        }
        export.running = true;
        self.folder = Some(export.options.folder.clone());
        let options = export.options.clone();
        let tell = export.progress.0.clone();
        let done = self.exports.0.clone();
        let job = self.job(&self.tabs[self.active], 0);
        std::thread::spawn(move || {
            let said = export::run(&job, &options, &seeds, &|p| {
                let _ = tell.send(p);
            })
            .map(|files| match files.as_slice() {
                [one] => format!("Exported {}", one.display()),
                many => format!("Exported {} editions to {}", many.len(), options.folder.display()),
            });
            let _ = done.send(said);
        });
    }

    /// The dialog itself.
    fn export_dialog(&mut self, ctx: &Context) {
        let Some(mut export) = self.export.take() else { return };
        while let Ok(p) = export.progress.1.try_recv() {
            export.said = Some((p.what, p.edition, p.editions, p.done));
        }
        let tab = &self.tabs[self.active];
        let renders = tab.summary.as_ref().map(|s| s.renders.clone()).unwrap_or_default();
        let kept = tab.history.iter().filter(|s| s.kept).count();
        let slug = export::slug(&self.job(tab, 0));
        let duration = tab.clip.as_ref().map_or(0.0, |c| c.duration);
        let asked = tab.full;

        let (mut close, mut start) = (false, false);
        let frame = egui::Frame::new()
            .fill(theme::PANEL)
            .stroke(Stroke::new(1.0_f32, theme::EDGE))
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(20, 16));
        let answer = egui::Modal::new(Id::new("export")).frame(frame).show(ctx, |ui| {
            ui.set_width(430.0);
            ui.label(RichText::new("Export").size(16.0).strong().color(theme::PAPER));
            ui.add_space(10.0);

            if renders.len() > 1 {
                ui.label(caption("WHAT"));
                ui.horizontal_wrapped(|ui| {
                    for kind in &renders {
                        let here = *kind == export.options.kind;
                        let text = RichText::new(kind).monospace().size(12.0).color(if here { theme::INK } else { theme::TEXT });
                        let button = egui::Button::new(text)
                            .fill(if here { theme::PAPER } else { theme::BAR })
                            .stroke(Stroke::new(1.0_f32, if here { theme::PAPER } else { theme::EDGE }));
                        let can = !Format::of(kind).is_empty();
                        if ui.add_enabled(can && !export.running, button).clicked() {
                            export.options.kind = kind.clone();
                            export.options.format = Format::of(kind)[0];
                        }
                    }
                });
                ui.add_space(10.0);
            }

            let formats = Format::of(&export.options.kind);
            if formats.len() > 1 {
                ui.label(caption("FORMAT"));
                ui.horizontal_wrapped(|ui| {
                    for format in formats {
                        let here = *format == export.options.format;
                        let ready = !format.needs_ffmpeg() || export::has_ffmpeg();
                        let text = RichText::new(format.name()).size(12.0).color(if here { theme::INK } else { theme::TEXT });
                        let button = egui::Button::new(text)
                            .fill(if here { theme::PAPER } else { theme::BAR })
                            .stroke(Stroke::new(1.0_f32, if here { theme::PAPER } else { theme::EDGE }));
                        let hint = match ready {
                            true => format!("Write a {}", format.name()),
                            false => "An MP4 needs ffmpeg, which is not on this machine".to_string(),
                        };
                        if ui.add_enabled(ready && !export.running, button).on_hover_text(hint).clicked() {
                            export.options.format = *format;
                        }
                    }
                });
                ui.add_space(10.0);
            }

            if export.options.format.sized() {
                ui.label(caption("SIZE"));
                ui.horizontal(|ui| {
                    let ratio = export.options.ratio();
                    let mut width = export.options.width as f64;
                    let mut height = export.options.height as f64;
                    fn field(value: &mut f64) -> egui::DragValue<'_> {
                        egui::DragValue::new(value).speed(4.0).range(16.0..=8192.0).fixed_decimals(0)
                    }
                    if ui.add_enabled(!export.running, field(&mut width)).changed() {
                        export.options.width = width as usize;
                        if export.options.keep_ratio {
                            export.options.height = ((width * ratio).round() as usize).max(16);
                        }
                    }
                    ui.label(RichText::new("×").color(theme::MUTED));
                    if ui.add_enabled(!export.running, field(&mut height)).changed() {
                        export.options.height = height as usize;
                        if export.options.keep_ratio {
                            export.options.width = ((height / ratio.max(0.01)).round() as usize).max(16);
                        }
                    }
                    ui.add_enabled(!export.running, egui::Checkbox::new(&mut export.options.keep_ratio, "keep proportions"));
                    let as_written = egui::Button::new(RichText::new("as written").small()).frame(false);
                    if ui.add_enabled(!export.running, as_written).on_hover_text(format!("{} × {}", asked.0, asked.1)).clicked()
                    {
                        (export.options.width, export.options.height) = asked;
                    }
                });
                ui.add_space(10.0);
            }

            if export.options.format == Format::Jpeg {
                ui.label(caption("QUALITY"));
                let mut quality = export.options.quality as f64;
                ui.spacing_mut().slider_width = 240.0;
                if ui.add_enabled(!export.running, egui::Slider::new(&mut quality, 40.0..=100.0).fixed_decimals(0)).changed() {
                    export.options.quality = quality as u8;
                }
                ui.add_space(10.0);
            }

            if export.options.format.moving() {
                ui.label(caption("FRAMES A SECOND"));
                ui.horizontal(|ui| {
                    let mut fps = export.options.fps;
                    if ui
                        .add_enabled(
                            !export.running,
                            egui::DragValue::new(&mut fps).speed(0.5).range(1.0..=60.0).fixed_decimals(0),
                        )
                        .changed()
                    {
                        export.options.fps = fps;
                    }
                    let frames = export.options.frames(duration);
                    ui.label(RichText::new(format!("{frames} frames · {duration:.1} s")).small().color(theme::MUTED));
                });
                ui.add_space(10.0);
            }

            ui.label(caption("EDITIONS"));
            ui.horizontal(|ui| {
                let this = egui::RadioButton::new(!export.kept, "this one");
                if ui.add_enabled(!export.running, this).clicked() {
                    export.kept = false;
                }
                let label = match kept {
                    0 => "none kept yet".to_string(),
                    1 => "the one kept".to_string(),
                    n => format!("the {n} kept"),
                };
                let all = egui::RadioButton::new(export.kept, label);
                if ui
                    .add_enabled(kept > 0 && !export.running, all)
                    .on_hover_text("Every edition marked Keep in this tab, one file each")
                    .clicked()
                {
                    export.kept = true;
                }
            });
            ui.add_space(10.0);

            ui.label(caption("FOLDER"));
            ui.horizontal(|ui| {
                let mut shown = export.options.folder.display().to_string();
                let field = egui::TextEdit::singleline(&mut shown).desired_width(300.0).font(FontId::monospace(12.0));
                if ui.add_enabled(!export.running, field).changed() {
                    export.options.folder = PathBuf::from(shown);
                }
                if ui.add_enabled(!export.running, egui::Button::new("Choose…")).clicked() {
                    if let Some(folder) = rfd::FileDialog::new().set_directory(&export.options.folder).pick_folder() {
                        export.options.folder = folder;
                    }
                }
            });
            ui.add_space(12.0);

            // What will be written, in the words of the files themselves.
            let seeds = match export.kept {
                true => kept,
                false => 1,
            };
            let first = export.options.file_name(&slug, self.tabs[self.active].seed);
            let line = match seeds {
                1 => first,
                n => format!("{first} and {} more", n - 1),
            };
            ui.add(egui::Label::new(RichText::new(line).monospace().small().color(theme::MUTED)).truncate());
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                match (&export.said, export.running) {
                    (Some((what, k, of, done)), true) => {
                        let text = match of {
                            1 => format!("{what}…"),
                            of => format!("edition {k} of {of} · {what}…"),
                        };
                        ui.add(
                            egui::ProgressBar::new(*done)
                                .desired_width(240.0)
                                .fill(theme::VERMILION)
                                .text(RichText::new(text).small()),
                        );
                    }
                    (None, true) => {
                        ui.add(
                            egui::ProgressBar::new(0.0)
                                .desired_width(240.0)
                                .fill(theme::VERMILION)
                                .text(RichText::new("drawing…").small()),
                        );
                    }
                    _ => {}
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let write = egui::Button::new(RichText::new("Export").strong().color(theme::INK)).fill(theme::PAPER);
                    if ui.add_enabled(!export.running, write).clicked() {
                        start = true;
                    }
                    if ui.add_enabled(!export.running, egui::Button::new("Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        });

        if answer.should_close() && !export.running {
            close = true;
        }
        self.export = (!close).then_some(export);
        if start {
            self.export_now();
        }
    }

    /// Takes what the engine and the export thread have finished.
    fn collect(&mut self, ctx: &Context) {
        while let Ok(reply) = self.engine.replies.try_recv() {
            let Some(tab) = self.tabs.iter_mut().find(|t| t.serial == reply.id && t.seed == reply.seed) else {
                continue;
            };
            tab.busy = !reply.last;
            match reply.outcome {
                Outcome::Image(p) => {
                    let version = tab.asked.as_ref().map_or(0, |a| a.0);
                    let image = ColorImage::from_rgb([p.width, p.height], &p.rgb);
                    tab.image = Some(ctx.load_texture(format!("edition-{}", tab.id), image, TextureOptions::LINEAR));
                    tab.image_px = (p.width, p.height);
                    tab.full = p.full;
                    tab.failure = None;
                    tab.note = None;
                    if reply.last {
                        tab.elapsed = Some(p.elapsed);
                        let thumb =
                            ctx.load_texture(format!("thumb-{}-{:x}", tab.id, tab.seed), thumbnail(&p), TextureOptions::LINEAR);
                        if let Some(shot) = tab.history.get_mut(tab.at) {
                            shot.thumb = Some((thumb, version));
                        }
                    }
                    tab.summary = Some(p.summary);
                    tab.follow();
                }
                Outcome::Clip(clip) => {
                    tab.clip = Some(Clip::new(clip.width, clip.height, clip.fps, clip.asked_fps, clip.duration, clip.frames));
                    tab.image_px = (clip.width, clip.height);
                    tab.full = clip.full;
                    tab.summary = Some(clip.summary);
                    tab.failure = None;
                    tab.note = None;
                }
                Outcome::Frame(frame) => {
                    let Some(clip) = &mut tab.clip else { continue };
                    let image = ColorImage::from_rgb([clip.width, clip.height], &frame.rgb);
                    let name = format!("frame-{}-{}", tab.id, frame.at);
                    clip.put(frame.at, ctx.load_texture(name, image, TextureOptions::LINEAR));
                    // The first frame of a loop stands in for it in the strip.
                    if frame.at == 0 {
                        let version = tab.asked.as_ref().map_or(0, |a| a.0);
                        let thumb = ColorImage::from_rgb([clip.width, clip.height], &frame.rgb);
                        let name = format!("thumb-{}-{:x}", tab.id, tab.seed);
                        let thumb = ctx.load_texture(name, thumb, TextureOptions::LINEAR);
                        if let Some(shot) = tab.history.get_mut(tab.at) {
                            shot.thumb = Some((thumb, version));
                        }
                    }
                }
                Outcome::Nothing(summary, what) => {
                    tab.summary = Some(summary);
                    tab.image = None;
                    tab.clip = None;
                    tab.failure = None;
                    tab.note = Some(what);
                    tab.follow();
                }
                // The last good picture stays on screen, dimmed, while the
                // code is broken.
                Outcome::Failed(f) => tab.failure = Some(f),
            }
        }
        while let Ok(done) = self.exports.1.try_recv() {
            self.export = None;
            match done {
                Ok(said) => self.say(said, false),
                Err(e) => self.say(format!("Export failed: {e}"), true),
            }
        }
        if self.notice.as_ref().is_some_and(|n| n.2.elapsed() > Duration::from_secs(8)) {
            self.notice = None;
        }
    }

    /// Moves the loop along, and keeps the window repainting while it plays.
    fn play(&mut self, ctx: &Context) {
        let tab = &mut self.tabs[self.active];
        let Some(clip) = &tab.clip else { return };
        if !tab.playing || clip.ready == 0 {
            return;
        }
        let step = Duration::from_secs_f64(1.0 / clip.fps.max(1.0));
        let dt = ctx.input(|i| i.stable_dt).min(0.2) as f64;
        tab.position = (tab.position + dt / clip.duration.max(0.01)).rem_euclid(1.0);
        ctx.request_repaint_after(step);
    }

    /// Asks for a new preview when the text, the seed or the viewport changed.
    fn request(&mut self, ctx: &Context) {
        let tab = &self.tabs[self.active];
        let doc = &self.docs[tab.doc];
        let want = (doc.version, tab.seed, self.side, tab.shown.clone());
        if tab.asked.as_ref() == Some(&want) {
            return;
        }
        // Only typing waits: a reroll should feel instant.
        let typing = tab.asked.as_ref().is_some_and(|a| a.0 != doc.version);
        let rested = doc.changed_at.elapsed();
        if typing && rested < DEBOUNCE {
            ctx.request_repaint_after(DEBOUNCE - rested);
            return;
        }
        self.serial += 1;
        let job = self.job(tab, self.serial);
        let tab = &mut self.tabs[self.active];
        tab.asked = Some(want);
        tab.serial = self.serial;
        tab.busy = true;
        self.engine.draw(job);
    }

    fn shortcuts(&mut self, ctx: &Context) {
        let cmd = |key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, key));
        if cmd(Key::R) || (!ctx.wants_keyboard_input() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space))) {
            self.tabs[self.active].reroll();
        }
        if cmd(Key::OpenBracket) {
            self.tabs[self.active].step(-1);
        }
        if cmd(Key::CloseBracket) {
            self.tabs[self.active].step(1);
        }
        if cmd(Key::S) {
            self.save();
        }
        if cmd(Key::O) {
            self.open_dialog();
        }
        if cmd(Key::E) {
            self.export();
        }
        if cmd(Key::T) {
            self.push_tab(self.tabs[self.active].doc);
        }
        if cmd(Key::W) {
            self.close(self.active);
        }
        let help = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) || cmd(Key::K);
        if help {
            self.open_reference(ctx);
        }
    }

    /// Opens the reference, on the name the cursor is on when it is on one.
    fn open_reference(&mut self, ctx: &Context) {
        if self.reference.is_some() {
            self.reference = None;
            return;
        }
        let doc = self.tabs[self.active].doc;
        let word = cursor_of(ctx, doc)
            .map(|index| {
                let text = &self.docs[doc].text;
                text[complete::word_at(text, byte_of_char(text, index))].to_string()
            })
            // A number under the cursor is not a name to look up.
            .filter(|word| word.starts_with(|c: char| c.is_alphabetic() || c == '_'));
        let shown = word.as_deref().and_then(reference::find).map(|e| e.name.clone());
        self.reference = Some(Reference { query: shown.clone().or(word).unwrap_or_default(), shown });
    }

    /// The knobs of the work on screen, read again when its text changes.
    fn knobs(&mut self) {
        let doc = self.tabs[self.active].doc;
        let want = (doc, self.docs[doc].version);
        if self.knobs_of != Some(want) {
            self.knobs = draws::knobs(&self.docs[doc].text);
            self.knobs_of = Some(want);
        }
    }

    /// Rewrites the work. Every control goes through here: the text is the
    /// only place a value is ever kept.
    fn rewrite(&mut self, edits: Vec<Edit>) {
        if edits.is_empty() {
            return;
        }
        let doc = &mut self.docs[self.tabs[self.active].doc];
        draws::apply(&mut doc.text, edits);
        doc.edited();
    }

    fn tab_title(&self, tab: &Tab) -> String {
        let doc = &self.docs[tab.doc];
        let name = match &tab.summary {
            Some(s) => s.name.clone(),
            None => doc.file_name().trim_end_matches(".alea").to_string(),
        };
        if doc.dirty() {
            format!("{name} •")
        } else {
            name
        }
    }

    fn top_bar(&mut self, ctx: &Context) {
        let frame = egui::Frame::new().fill(theme::BAR).inner_margin(egui::Margin::symmetric(12, 6));
        egui::TopBottomPanel::top("tabs").frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("alea").monospace().strong().size(15.0).color(theme::PAPER));
                ui.add_space(10.0);

                let (mut select, mut close) = (None, None);
                for (k, tab) in self.tabs.iter().enumerate() {
                    let on = k == self.active;
                    let mut text = LayoutJob::default();
                    let ink = if on { theme::PAPER } else { theme::MUTED };
                    text.append(&self.tab_title(tab), 0.0, TextFormat::simple(FontId::proportional(13.0), ink));
                    let seed = if on { theme::BLUSH } else { theme::FAINT };
                    text.append(&format_seed(tab.seed), 10.0, TextFormat::simple(FontId::monospace(11.0), seed));
                    let stroke = if on { Stroke::new(1.0_f32, theme::VERMILION) } else { Stroke::new(1.0_f32, theme::LINE) };
                    let button = egui::Button::new(text).fill(if on { theme::INK } else { theme::BAR }).stroke(stroke);
                    let shown = ui.add(button).on_hover_text(
                        self.docs[tab.doc].path.as_ref().map_or("Not saved yet".into(), |p| p.display().to_string()),
                    );
                    if shown.clicked() {
                        select = Some(k);
                    }
                    if shown.middle_clicked() {
                        close = Some(k);
                    }
                    shown.context_menu(|ui| {
                        if ui.button("Close tab").clicked() {
                            close = Some(k);
                            ui.close();
                        }
                    });
                }
                if ui.button("+").on_hover_text("This work in a new tab, at another seed").clicked() {
                    self.push_tab(self.tabs[self.active].doc);
                }
                if let Some(k) = select {
                    self.active = k;
                }
                if let Some(k) = close {
                    self.close(k);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let export = egui::Button::new(RichText::new("Export…").strong().color(theme::INK)).fill(theme::PAPER);
                    let shown = self.tabs[self.active].shown.clone();
                    let can = self.tabs[self.active].summary.as_ref().is_some_and(|s| s.renders.contains(&shown));
                    let hint = "What this work renders, in the format and size you choose";
                    if ui.add_enabled(can && self.export.is_none(), export).on_hover_text(hint).clicked() {
                        self.export();
                    }
                    if ui.button("Save").clicked() {
                        self.save();
                    }
                    if ui.button("Open").clicked() {
                        self.open_dialog();
                    }
                    if let Some((text, bad, _)) = &self.notice {
                        ui.add(
                            egui::Label::new(RichText::new(text).color(if *bad { theme::BLUSH } else { theme::MOSS })).truncate(),
                        );
                    }
                });
            });
        });
    }

    fn seed_strip(&mut self, ctx: &Context) {
        let version = self.docs[self.tabs[self.active].doc].version;
        let tab = &mut self.tabs[self.active];
        let frame = egui::Frame::new().fill(theme::BAR).inner_margin(egui::Margin::symmetric(16, 10));
        egui::TopBottomPanel::bottom("seed").frame(frame).exact_height(128.0).show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.label(caption("EDITION"));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let field = egui::TextEdit::singleline(&mut tab.seed_text)
                            .font(FontId::monospace(18.0))
                            .text_color(theme::BLUSH)
                            .desired_width(132.0)
                            .margin(egui::Margin::symmetric(10, 8));
                        let field = egui::Frame::new()
                            .stroke(Stroke::new(2.0_f32, theme::VERMILION))
                            .corner_radius(4)
                            .show(ui, |ui| ui.add(field))
                            .inner;
                        if field.lost_focus() {
                            match parse_seed(&tab.seed_text) {
                                Some(seed) => tab.go(seed),
                                None => tab.seed_text = format_seed(tab.seed),
                            }
                        }
                        field.on_hover_text("The seed names this edition: type one to return to it");

                        let tall = Vec2::new(30.0, 38.0);
                        if ui
                            .add_enabled(tab.at > 0, egui::Button::new("◀").min_size(tall))
                            .on_hover_text("Previous edition")
                            .clicked()
                        {
                            tab.step(-1);
                        }
                        if ui
                            .add_enabled(tab.at + 1 < tab.history.len(), egui::Button::new("▶").min_size(tall))
                            .on_hover_text("Next edition")
                            .clicked()
                        {
                            tab.step(1);
                        }
                        let reroll = egui::Button::new(RichText::new("Reroll").strong().color(theme::INK))
                            .fill(theme::VERMILION)
                            .min_size(Vec2::new(84.0, 38.0));
                        if ui.add(reroll).on_hover_text("A new edition (Space, when the code is not focused)").clicked() {
                            tab.reroll();
                        }
                        let kept = tab.history[tab.at].kept;
                        let keep =
                            egui::Button::new(if kept { "Kept" } else { "Keep" }).selected(kept).min_size(Vec2::new(56.0, 38.0));
                        if ui.add(keep).on_hover_text("Kept editions stay in the history").clicked() {
                            tab.history[tab.at].kept = !kept;
                        }
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                let mut visit = None;
                egui::ScrollArea::horizontal().stick_to_right(true).auto_shrink(false).show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for (k, shot) in tab.history.iter().enumerate() {
                            let here = k == tab.at;
                            let (rect, hit) = ui.allocate_exact_size(Vec2::new(70.0, 92.0), Sense::click());
                            let frame = Rect::from_min_size(rect.min, Vec2::new(70.0, 74.0));
                            ui.painter().rect_filled(frame, 3, theme::WELL);
                            if let Some((thumb, drawn)) = &shot.thumb {
                                // Drawn from older code: shown faded until revisited.
                                let tint = if *drawn == version { Color32::WHITE } else { Color32::from_gray(90) };
                                let fit = fit(thumb.size_vec2(), frame.shrink(2.0));
                                ui.painter().image(thumb.id(), fit, UV, tint);
                            }
                            let edge =
                                if here { Stroke::new(2.0_f32, theme::VERMILION) } else { Stroke::new(1.0_f32, theme::LINE) };
                            ui.painter().rect_stroke(frame, 3, edge, egui::StrokeKind::Inside);
                            if shot.kept {
                                ui.painter().circle_filled(frame.right_top() + Vec2::new(-9.0, 9.0), 4.0, theme::AMBER);
                            }
                            let ink = if here { theme::BLUSH } else { theme::MUTED };
                            ui.painter().text(
                                rect.center_bottom(),
                                Align2::CENTER_BOTTOM,
                                format_seed(shot.seed),
                                FontId::monospace(11.0),
                                ink,
                            );
                            if hit.on_hover_text("Return to this edition").clicked() {
                                visit = Some(k);
                            }
                        }
                    });
                });
                if let Some(k) = visit {
                    tab.revisit(k);
                }
            });
        });
    }

    fn code_pane(&mut self, ctx: &Context) {
        // The list of names takes these keys before the editor sees them.
        let mut key = Key2::None;
        if self.offer.is_some() {
            ctx.input_mut(|i| {
                key = if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    Key2::Move(1)
                } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    Key2::Move(-1)
                } else if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                    Key2::Accept
                } else if i.consume_key(Modifiers::NONE, Key::Escape) {
                    Key2::Close
                } else {
                    Key2::None
                };
            });
        }
        let asked = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space));
        match key {
            Key2::Move(by) => {
                if let Some(offer) = &mut self.offer {
                    let n = offer.items.len() as isize;
                    offer.at = (offer.at as isize + by).rem_euclid(n.max(1)) as usize;
                }
            }
            Key2::Close => self.offer = None,
            Key2::Accept => self.accept(ctx),
            Key2::None => {}
        }

        let doc_index = self.tabs[self.active].doc;
        let id = Id::new(("code", doc_index));
        let failure = self.tabs[self.active].failure.clone();
        let (busy, elapsed) = (self.tabs[self.active].busy, self.tabs[self.active].elapsed);
        let frame = egui::Frame::new().fill(theme::PANEL);

        egui::SidePanel::left("code").frame(frame).default_width(470.0).width_range(280.0..=760.0).show(ctx, |ui| {
            let mut help = false;
            header(ui, "CODE", |ui| {
                let (text, ink) = match (&failure, busy, elapsed) {
                    (Some(_), _, _) => ("error".to_string(), theme::BLUSH),
                    (None, true, _) => ("drawing…".to_string(), theme::AMBER),
                    (None, false, Some(t)) => (format!("live · {} ms", t.as_millis()), theme::MOSS),
                    _ => (String::new(), theme::MUTED),
                };
                ui.label(RichText::new(text).small().color(ink));
                help = ui.add(egui::Button::new("?").small()).on_hover_text("The reference (F1)").clicked();
            });
            if help {
                self.open_reference(ui.ctx());
            }

            if let Some(failure) = &failure {
                let frame = egui::Frame::new().fill(Color32::from_rgb(0x2a, 0x1b, 0x16)).inner_margin(12);
                egui::TopBottomPanel::bottom("diagnostic").frame(frame).show_inside(ui, |ui| {
                    egui::ScrollArea::both().max_height(120.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(failure.text.trim_end()).monospace().color(theme::BLUSH)).extend());
                    });
                });
            }
            self.reference_drawer(ui);

            let font = FontId::monospace(13.0);
            let row = ui.fonts_mut(|f| f.row_height(&font));
            let lines = self.docs[doc_index].text.matches('\n').count() + 1;
            let gutter = 46.0;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // The line numbers stay put while the code scrolls sideways.
                let (numbers, _) = ui.allocate_exact_size(Vec2::new(gutter, ui.available_height()), Sense::hover());
                let area = egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
                        ui.painter().layout_job(highlight(text.as_str(), &font))
                    };
                    let out = egui::TextEdit::multiline(&mut self.docs[doc_index].text)
                        .id(id)
                        .frame(false)
                        .desired_width(f32::INFINITY)
                        .desired_rows(lines.max(24))
                        .margin(egui::Margin::symmetric(8, 12))
                        .lock_focus(true)
                        .layouter(&mut layouter)
                        .show(ui);
                    (out.response.clone(), out.galley.clone(), out.galley_pos, out.cursor_range)
                });
                let (response, galley, galley_pos, cursor) = area.inner;
                if response.changed() {
                    self.docs[doc_index].edited();
                }

                let bad = failure.as_ref().and_then(|f| f.line);
                let ink = ui.painter().with_clip_rect(numbers.intersect(ui.clip_rect()));
                for n in 1..=lines {
                    let y = galley_pos.y + row * (n - 1) as f32;
                    if y < numbers.top() - row || y > numbers.bottom() {
                        continue;
                    }
                    let wrong = bad == Some(n);
                    if wrong {
                        let line = Rect::from_min_size(egui::pos2(numbers.left(), y), Vec2::new(ui.clip_rect().width(), row));
                        ui.painter().rect_filled(line, 0, Color32::from_rgba_unmultiplied(0xf0, 0x69, 0x4c, 34));
                        ui.painter().rect_filled(Rect::from_min_size(line.min, Vec2::new(2.0_f32, row)), 0, theme::VERMILION);
                    }
                    let colour = if wrong { theme::BLUSH } else { theme::FAINT };
                    ink.text(egui::pos2(numbers.right() - 14.0, y), Align2::RIGHT_TOP, n, font.clone(), colour);
                }

                // What the pointer rests on, straight from the reference.
                if let (true, Some(at)) = (response.hovered(), ui.ctx().pointer_latest_pos()) {
                    let text = &self.docs[doc_index].text;
                    let index = galley.cursor_from_pos(at - galley_pos).index;
                    let word = &text[complete::word_at(text, byte_of_char(text, index))];
                    if let Some(entry) = reference::find(word) {
                        let anchor = egui::PopupAnchor::Pointer;
                        egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), Id::new("word-help"), anchor)
                            .show(|ui| entry_card(ui, entry, true));
                    }
                }

                // What to offer for the name being typed.
                match (response.has_focus(), cursor.map(|c| c.primary.index)) {
                    (true, Some(index)) => {
                        let text = &self.docs[doc_index].text;
                        let at = byte_of_char(text, index);
                        let word = complete::word_at(text, at);
                        let typed = !text[word.start..at].is_empty();
                        if typed || asked || self.offer.as_ref().is_some_and(|o| o.word.start == word.start) {
                            let items = complete::candidates(text, at);
                            let same = self.offer.as_ref().is_some_and(|o| o.word.start == word.start);
                            let kept = self.offer.as_ref().map_or(0, |o| o.at);
                            let under = galley_pos + galley.pos_from_cursor(CCursor::new(index)).left_bottom().to_vec2();
                            self.offer = (!items.is_empty()).then(|| Offer {
                                at: if same { kept.min(items.len() - 1) } else { 0 },
                                items,
                                word: word.start..at,
                                under,
                            });
                        } else {
                            self.offer = None;
                        }
                    }
                    _ => self.offer = None,
                }
            });
        });
    }

    /// Puts the name the list has settled on into the work.
    fn accept(&mut self, ctx: &Context) {
        let Some(offer) = self.offer.take() else { return };
        let Some(item) = offer.items.get(offer.at) else { return };
        let doc_index = self.tabs[self.active].doc;
        let doc = &mut self.docs[doc_index];
        if offer.word.end > doc.text.len() {
            return;
        }
        doc.text.replace_range(offer.word.clone(), &item.insert);
        doc.edited();
        let at = char_of_byte(&doc.text, offer.word.start + item.insert.len());
        let id = Id::new(("code", doc_index));
        if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
            egui::TextEdit::store_state(ctx, id, state);
        }
    }

    /// The list of names, under the cursor.
    fn offer_list(&mut self, ctx: &Context) {
        let Some(offer) = &self.offer else { return };
        let (items, at, under) = (offer.items.clone(), offer.at, offer.under);
        let mut chosen = None;
        egui::Area::new(Id::new("offer")).order(egui::Order::Foreground).fixed_pos(under + Vec2::new(0.0, 4.0)).show(ctx, |ui| {
            let frame =
                egui::Frame::new().fill(theme::BAR).stroke(Stroke::new(1.0_f32, theme::EDGE)).corner_radius(5).inner_margin(4);
            frame.show(ui, |ui| {
                ui.set_max_width(460.0);
                ui.spacing_mut().item_spacing.y = 1.0;
                for (k, item) in items.iter().enumerate() {
                    let here = k == at;
                    let fill = if here { Color32::from_rgb(0x34, 0x32, 0x2e) } else { Color32::TRANSPARENT };
                    let row = egui::Frame::new().fill(fill).corner_radius(3).inner_margin(egui::Margin::symmetric(6, 3));
                    let hit = row
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(item.mark).monospace().small().color(theme::FAINT));
                                ui.label(RichText::new(&item.name).monospace().color(if here {
                                    theme::PAPER
                                } else {
                                    theme::TEXT
                                }));
                                ui.add(egui::Label::new(RichText::new(&item.detail).small().color(theme::MUTED)).truncate());
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if hit.clicked() {
                        chosen = Some(k);
                    }
                }
                if let Some(item) = items.get(at).filter(|i| !i.summary.is_empty()) {
                    ui.add_space(2.0);
                    ui.separator();
                    ui.add(egui::Label::new(RichText::new(&item.summary).small().color(theme::MUTED)).wrap());
                }
            });
        });
        if let Some(k) = chosen {
            if let Some(offer) = &mut self.offer {
                offer.at = k;
            }
            self.accept(ctx);
        }
    }

    /// The reference, under the code.
    fn reference_drawer(&mut self, ui: &mut egui::Ui) {
        let Some(mut state) = self.reference.take() else { return };
        let mut close = false;
        let frame = egui::Frame::new().fill(theme::BAR).inner_margin(egui::Margin::symmetric(12, 8));
        egui::TopBottomPanel::bottom("reference").frame(frame).resizable(true).default_height(280.0).show_inside(ui, |ui| {
            // The drawer lives inside the code panel: it takes the width it
            // is given and never asks for more.
            ui.set_max_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(caption("REFERENCE"));
                let field =
                    egui::TextEdit::singleline(&mut state.query).hint_text("a name, or what it does").desired_width(170.0);
                ui.add(field);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.button("Close").clicked();
                });
            });
            ui.add_space(6.0);

            let found = reference::search(&state.query);
            let list = (ui.available_width() * 0.36).clamp(110.0, 220.0);
            ui.horizontal_top(|ui| {
                egui::ScrollArea::vertical().id_salt("entries").auto_shrink(false).max_width(list).show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.set_min_width(list - 16.0);
                        for entry in found.iter().take(400) {
                            let here = state.shown.as_deref() == Some(entry.name.as_str());
                            let ink = if here { theme::BLUSH } else { theme::TEXT };
                            let mut text = LayoutJob::default();
                            text.append(entry.family.mark(), 0.0, TextFormat::simple(FontId::monospace(11.0), theme::FAINT));
                            text.append(&entry.name, 8.0, TextFormat::simple(FontId::monospace(13.0), ink));
                            if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                state.shown = Some(entry.name.clone());
                            }
                        }
                        if found.is_empty() {
                            ui.label(RichText::new("Nothing of that name.").color(theme::MUTED));
                        }
                    });
                });
                ui.separator();
                let shown = state.shown.as_deref().and_then(reference::find).or_else(|| found.first().copied());
                egui::ScrollArea::vertical().id_salt("detail").auto_shrink(false).show(ui, |ui| {
                    ui.vertical(|ui| {
                        if let Some(entry) = shown {
                            entry_card(ui, entry, false);
                        }
                    });
                });
            });
        });
        self.reference = (!close).then_some(state);
    }

    fn controls_pane(&mut self, ctx: &Context) {
        self.knobs();
        let doc_index = self.tabs[self.active].doc;
        let source = self.docs[doc_index].text.clone();
        let knobs = std::mem::take(&mut self.knobs);
        let draws = knobs.iter().filter(|k| k.is_draw()).count();
        let mut row = Row {
            editing: self.editing.take(),
            editing_choice: self.editing_choice.take(),
            selected: self.selected,
            reveal: None,
            edits: Vec::new(),
        };

        let frame = egui::Frame::new().fill(theme::PANEL);
        egui::SidePanel::right("controls").frame(frame).default_width(344.0).width_range(260.0..=560.0).show(ctx, |ui| {
            header(ui, "CONTROLS", |ui| {
                let text = match knobs.len() {
                    0 => "nothing to tune".to_string(),
                    n => format!("{draws} draws · {} numbers", n - draws),
                };
                ui.label(RichText::new(text).small().color(theme::MUTED));
            });
            self.facts_panel(ui);

            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                if knobs.is_empty() {
                    ui.add_space(14.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(16.0);
                        let text = "Every choice, draw and number of a work that reads correctly turns up here.";
                        ui.add(egui::Label::new(RichText::new(text).color(theme::MUTED)).wrap());
                    });
                }
                for (title, want_draw) in [("DRAWS IN THIS EDITION", true), ("NUMBERS IN THE CODE", false)] {
                    let section: Vec<(usize, &Knob)> =
                        knobs.iter().enumerate().filter(|(_, k)| k.is_draw() == want_draw).collect();
                    if section.is_empty() {
                        continue;
                    }
                    let held = section
                        .iter()
                        .filter(|(_, k)| matches!(&k.kind, Kind::Choose { options, .. } if draws::held_at(options).is_some()))
                        .count();
                    ui.add_space(6.0);
                    egui::Frame::new().fill(theme::BAR).inner_margin(egui::Margin::symmetric(16, 6)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(caption(title));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let (text, ink) = match (want_draw, held) {
                                    (true, 0) => ("the seed decides these".to_string(), theme::FAINT),
                                    (true, n) => (format!("{n} held"), theme::BLUSH),
                                    _ => ("written once".to_string(), theme::FAINT),
                                };
                                ui.label(RichText::new(text).small().color(ink));
                            });
                        });
                    });
                    let mut group = String::new();
                    for (k, knob) in section {
                        if group != knob.group {
                            group = knob.group.clone();
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(RichText::new(&knob.group).color(theme::MUTED));
                            });
                            ui.add_space(2.0);
                        }
                        knob_row(ui, k, knob, &source, &mut row);
                    }
                }
                ui.add_space(16.0);
            });
        });

        self.knobs = knobs;
        self.editing = row.editing;
        self.editing_choice = row.editing_choice;
        self.selected = row.selected;
        if let Some(at) = row.reveal {
            self.reveal(ctx, at);
        }
        self.rewrite(row.edits);
    }

    /// Puts the cursor on the piece of code a control belongs to, selected,
    /// so the pane and the text always say the same thing.
    fn reveal(&mut self, ctx: &Context, at: std::ops::Range<usize>) {
        let doc_index = self.tabs[self.active].doc;
        let text = &self.docs[doc_index].text;
        if at.end > text.len() {
            return;
        }
        let (from, to) = (char_of_byte(text, at.start), char_of_byte(text, at.end));
        let id = Id::new(("code", doc_index));
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(from), CCursor::new(to))));
        egui::TextEdit::store_state(ctx, id, state);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    /// What the edition is, under the controls.
    fn facts_panel(&mut self, ui: &mut egui::Ui) {
        let tab = &self.tabs[self.active];
        let (seed, summary, full, at, history) = (tab.seed, tab.summary.clone(), tab.full, tab.at, tab.history.len());
        let file = self.docs[tab.doc].file_name();
        let frame = egui::Frame::new().fill(theme::BAR).inner_margin(egui::Margin::symmetric(14, 10));
        egui::TopBottomPanel::bottom("facts").frame(frame).show_inside(ui, |ui| {
            egui::Grid::new("facts").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
                let fact = |ui: &mut egui::Ui, name: &str, value: String, ink: Color32| {
                    ui.label(RichText::new(name).small().color(theme::MUTED));
                    ui.label(RichText::new(value).monospace().small().color(ink));
                    ui.end_row();
                };
                if let Some(s) = &summary {
                    fact(ui, "work", s.name.clone(), theme::PAPER);
                    fact(ui, "renders", s.renders.join(", "), theme::PAPER);
                    if s.attempts > 1 {
                        fact(ui, "attempts", s.attempts.to_string(), theme::TEXT);
                    }
                }
                fact(ui, "seed", format_seed(seed), theme::BLUSH);
                if full.0 > 0 {
                    fact(ui, "exports", format!("{} × {}", full.0, full.1), theme::TEXT);
                }
                let _ = (at, history);
            });
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    RichText::new(format!("alea {file} --seed {}", format_seed(seed))).monospace().small().color(theme::MUTED),
                )
                .truncate(),
            );
        });
    }

    fn viewport(&mut self, ctx: &Context) {
        let mut show = None;
        let tab = &self.tabs[self.active];
        let (shown, playing) = (tab.shown.clone(), tab.playing);
        let mut play = None;
        let mut seek = None;

        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::WELL)).show(ctx, |ui| {
            let tab = &self.tabs[self.active];
            header(ui, "RENDERS", |ui| {
                match (&tab.clip, &tab.image) {
                    (Some(clip), _) => {
                        let text = format!("{} × {} · {:.0} fps · {:.1} s", tab.full.0, tab.full.1, clip.fps, clip.duration);
                        ui.label(RichText::new(text).small().monospace().color(theme::MUTED));
                    }
                    (None, Some(_)) => {
                        let zoom = 100.0 * tab.image_px.0 as f32 / tab.full.0.max(1) as f32;
                        let text = format!("{} × {} · preview at {zoom:.0} %", tab.full.0, tab.full.1);
                        ui.label(RichText::new(text).small().monospace().color(theme::MUTED));
                    }
                    _ => {}
                }
                if let Some(s) = &tab.summary {
                    for kind in s.renders.iter().rev() {
                        let here = *kind == shown;
                        let text = RichText::new(kind).small().strong().color(if here { theme::INK } else { theme::TEXT });
                        let chip = egui::Button::new(text)
                            .fill(if here { theme::PAPER } else { theme::WELL })
                            .stroke(Stroke::new(1.0_f32, if here { theme::PAPER } else { theme::EDGE }))
                            .corner_radius(10);
                        if ui.add(chip).on_hover_text(format!("Show `render {kind}`")).clicked() {
                            show = Some(kind.clone());
                        }
                    }
                }
            });

            // A loop plays under its own transport.
            if tab.clip.is_some() && shown == "video" {
                let frame = egui::Frame::new().fill(theme::INK).inner_margin(egui::Margin::symmetric(14, 8));
                egui::TopBottomPanel::bottom("transport").frame(frame).show_inside(ui, |ui| {
                    let tab = &self.tabs[self.active];
                    let clip = tab.clip.as_ref().expect("a clip");
                    ui.horizontal(|ui| {
                        let mark = if playing { "Hold" } else { "Play" };
                        let button = egui::Button::new(RichText::new(mark).strong().color(theme::INK))
                            .fill(theme::PAPER)
                            .min_size(Vec2::new(54.0, 28.0));
                        if ui.add(button).on_hover_text("Play or hold the loop").clicked() {
                            play = Some(!playing);
                        }
                        let mut position = tab.position;
                        ui.spacing_mut().slider_width = (ui.available_width() - 150.0).max(80.0);
                        ui.visuals_mut().selection.bg_fill = theme::VERMILION;
                        ui.visuals_mut().widgets.inactive.bg_fill = theme::LINE;
                        let scrub = egui::Slider::new(&mut position, 0.0..=1.0).show_value(false);
                        if ui.add(scrub).changed() {
                            seek = Some(position);
                        }
                        let text = match clip.done() {
                            true => format!("{:.1} / {:.1} s", position * clip.duration, clip.duration),
                            false => format!("drawing {}/{}", clip.ready, clip.frames.len()),
                        };
                        let ink = if clip.done() { theme::MUTED } else { theme::AMBER };
                        ui.label(RichText::new(text).small().monospace().color(ink));
                    });
                });
            }

            let area = ui.available_rect_before_wrap().shrink(28.0);
            // What the viewport can show decides the size of the preview.
            let px = (area.width().min(area.height()) * ctx.pixels_per_point()).max(64.0) as usize;
            self.side = px.div_ceil(128) * 128;

            let tab = &self.tabs[self.active];
            let dim = tab.failure.is_some();
            let shot = match (shown.as_str(), &tab.clip, &tab.image) {
                ("video", Some(clip), _) => clip.at(tab.position).map(|frame| (frame, clip.width, clip.height)),
                (_, _, Some(image)) => Some((image, tab.image_px.0, tab.image_px.1)),
                _ => None,
            };
            match (shot, &tab.note) {
                (Some((texture, w, h)), _) => {
                    let rect = fit(Vec2::new(w as f32, h as f32), area);
                    let tint = if dim { Color32::from_gray(110) } else { Color32::WHITE };
                    ui.painter().rect_filled(rect.expand(1.0), 0, theme::LINE);
                    ui.painter().image(texture.id(), rect, UV, tint);
                }
                (None, Some(note)) if !dim => {
                    ui.painter().text(area.center(), Align2::CENTER_CENTER, note, FontId::proportional(14.0), theme::MUTED);
                }
                _ => {
                    let text = if dim { "Nothing to show until the work reads correctly." } else { "Drawing…" };
                    ui.painter().text(area.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), theme::MUTED);
                }
            }
        });

        let tab = &mut self.tabs[self.active];
        if let Some(kind) = show {
            tab.shown = kind;
            tab.position = 0.0;
        }
        if let Some(on) = play {
            tab.playing = on;
        }
        if let Some(position) = seek {
            tab.position = position;
            tab.playing = false;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.collect(ctx);
        self.shortcuts(ctx);
        self.top_bar(ctx);
        self.seed_strip(ctx);
        self.code_pane(ctx);
        self.controls_pane(ctx);
        self.viewport(ctx);
        self.offer_list(ctx);
        self.export_dialog(ctx);
        self.play(ctx);
        self.request(ctx);

        let tab = &self.tabs[self.active];
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} {} · alea", self.tab_title(tab), format_seed(tab.seed))));
    }
}

/// The whole texture.
const UV: Rect = Rect { min: egui::pos2(0.0, 0.0), max: egui::pos2(1.0, 1.0) };

fn caption(text: &str) -> RichText {
    RichText::new(text).small().color(theme::MUTED)
}

/// The strip at the top of each pane: its name on the left, `right` on the right.
fn header(ui: &mut egui::Ui, name: &str, right: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(caption(name));
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        });
    });
    let line = ui.available_rect_before_wrap();
    ui.painter().hline(line.x_range(), line.top(), Stroke::new(1.0_f32, theme::LINE));
}

/// The largest rectangle of proportions `size` centred in `area`.
fn fit(size: Vec2, area: Rect) -> Rect {
    let scale = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(area.center(), size * scale)
}

/// Colours alea source for the code pane, with Qell's highlighter.
fn highlight(text: &str, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let plain = |ink| TextFormat::simple(font.clone(), ink);
    let mut at = 0;
    for span in qell_core::alea::highlight(text) {
        if span.start > at {
            job.append(&text[at..span.start], 0.0, plain(theme::TEXT));
        }
        let token = &text[span.start..span.end];
        job.append(token, 0.0, plain(theme::token(span.kind, token)));
        at = span.end;
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, plain(theme::TEXT));
    }
    job
}

/// A small copy of a preview for the history strip.
fn thumbnail(p: &Preview) -> ColorImage {
    let scale = (THUMB as f32 / p.width.max(p.height) as f32).min(1.0);
    let (w, h) = (((p.width as f32 * scale) as usize).max(1), ((p.height as f32 * scale) as usize).max(1));
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (x * p.width / w, y * p.height / h);
            let k = (sy * p.width + sx) * 3;
            rgb.extend_from_slice(&p.rgb[k..k + 3]);
        }
    }
    ColorImage::from_rgb([w, h], &rgb)
}

/// Where the cursor is in the code of `doc`, counted in characters.
fn cursor_of(ctx: &Context, doc: usize) -> Option<usize> {
    egui::TextEdit::load_state(ctx, Id::new(("code", doc)))?.cursor.char_range().map(|r| r.primary.index)
}

/// egui counts characters; the source is indexed in bytes.
fn byte_of_char(text: &str, index: usize) -> usize {
    text.char_indices().nth(index).map_or(text.len(), |(i, _)| i)
}

fn char_of_byte(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].chars().count()
}

/// What the reference says about one name.
fn entry_card(ui: &mut egui::Ui, entry: &Entry, compact: bool) {
    let width = if compact { 420.0 } else { ui.available_width() };
    ui.set_max_width(width.max(160.0));
    ui.vertical(|ui| {
        ui.add(egui::Label::new(RichText::new(&entry.signature).monospace().color(theme::PAPER)).wrap());
        ui.label(RichText::new(&entry.section).small().color(theme::FAINT));
        if !entry.summary.is_empty() {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&entry.summary).color(theme::TEXT)).wrap());
        }
        if !compact && !entry.params().is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(format!("parameters: {}", entry.params().join(", "))).small().color(theme::MUTED));
        }
    });
}

/// What a row of controls may hand back to the pane.
struct Row {
    editing: Option<Editing>,
    editing_choice: Option<usize>,
    selected: Option<usize>,
    /// A piece of code to put the cursor on.
    reveal: Option<std::ops::Range<usize>>,
    edits: Vec<Edit>,
}

/// One number as a slider and its value, in the shape the code writes it.
/// The value shown is read out of the source every frame; moving the
/// handle writes a new one back.
fn slider(ui: &mut egui::Ui, id: (usize, usize), num: &Num, accent: bool) -> Option<Edit> {
    let mut value = num.value;
    let widget = egui::Slider::new(&mut value, num.track())
        .fixed_decimals(num.decimals)
        .clamping(egui::SliderClamping::Never)
        .trailing_fill(true)
        .show_value(true);
    let room = (ui.available_width() - 72.0).clamp(60.0, 210.0);
    let shown = ui
        .push_id(id, |ui| {
            ui.spacing_mut().slider_width = room;
            // The part of the track behind the handle: the row the hand is
            // on shows it in amber, the others in grey.
            ui.visuals_mut().selection.bg_fill = if accent { theme::AMBER } else { theme::MUTED };
            ui.visuals_mut().widgets.inactive.bg_fill = theme::LINE;
            ui.add(widget)
        })
        .inner;
    (shown.changed() && value != num.value).then(|| Edit { at: num.at.clone(), text: num.text(value) })
}

/// The options of a choice, side by side. Clicking one holds the choice
/// there; clicking the one it is held at lets it go again.
fn segmented(ui: &mut egui::Ui, id: usize, options: &[Opt], held: Option<usize>, can_hold: bool) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(2.0, 3.0);
        for (k, option) in options.iter().enumerate() {
            let on = held == Some(k);
            let shown = option.text.trim_matches('"');
            let text = RichText::new(shown).monospace().size(12.0).color(if on { theme::INK } else { theme::TEXT });
            let button = egui::Button::new(text)
                .fill(if on { theme::PAPER } else { theme::BAR })
                .stroke(Stroke::new(1.0_f32, if on { theme::PAPER } else { theme::EDGE }))
                .corner_radius(3)
                .min_size(Vec2::new(30.0, 24.0));
            let hint = match (can_hold, on) {
                (false, _) => "This choice is weighted in the code".to_string(),
                (true, true) => "Let this choice go: every edition draws its own again".to_string(),
                (true, false) => format!("Hold this choice at {shown} for every edition"),
            };
            if ui.push_id((id, k), |ui| ui.add_enabled(can_hold, button)).inner.on_hover_text(hint).clicked() {
                clicked = Some(k);
            }
        }
    });
    clicked
}

/// One row of the controls: a choice, a range, or a number.
fn knob_row(ui: &mut egui::Ui, index: usize, knob: &Knob, source: &str, row: &mut Row) {
    let here = row.selected == Some(knob.at);
    let frame = egui::Frame::new()
        .fill(if here { Color32::from_rgb(0x26, 0x24, 0x1f) } else { Color32::TRANSPARENT })
        .inner_margin(egui::Margin { left: 16, right: 14, top: 3, bottom: 3 });
    let mut take_me_there = false;
    let mut name = |ui: &mut egui::Ui, text: RichText, width: f32| {
        let label = egui::Label::new(text).truncate().halign(Align::LEFT).sense(Sense::click());
        let shown = match width > 0.0 {
            true => ui.add_sized(Vec2::new(width, 20.0), label),
            false => ui.add(label),
        };
        if shown.on_hover_text("Show this in the code").clicked() {
            take_me_there = true;
        }
    };
    frame.show(ui, |ui| {
        if here {
            let bar = Rect::from_min_size(
                ui.max_rect().left_top() - Vec2::new(16.0, 3.0),
                Vec2::new(2.0, ui.max_rect().height() + 6.0),
            );
            ui.painter().rect_filled(bar, 0, theme::AMBER);
        }
        match &knob.kind {
            Kind::Number(num) => {
                ui.horizontal(|ui| {
                    name(ui, RichText::new(&knob.label).color(if here { theme::PAPER } else { theme::TEXT }), 62.0);
                    if let Some(edit) = slider(ui, (index, 0), num, here) {
                        row.edits.push(edit);
                    }
                });
            }
            Kind::Spread { dist, per_point, names, from, to } => {
                ui.horizontal(|ui| {
                    name(ui, RichText::new(&knob.label).color(theme::PAPER), 0.0);
                    let draw = format!("{}{dist}", if *per_point { "~~" } else { "~" });
                    ui.label(RichText::new(draw).monospace().small().color(theme::VERMILION));
                });
                for (slot, (name, num)) in [(names[0], from), (names[1], to)].into_iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            Vec2::new(62.0, 20.0),
                            egui::Label::new(RichText::new(name).small().color(theme::MUTED)).halign(Align::LEFT),
                        );
                        if let Some(edit) = slider(ui, (index, slot + 1), num, here) {
                            row.edits.push(edit);
                        }
                    });
                }
            }
            Kind::Choose { options, body } => {
                let held = draws::held_at(options);
                let can_hold = draws::can_hold(options);
                let editing = row.editing_choice == Some(knob.at);
                ui.horizontal(|ui| {
                    name(ui, RichText::new(&knob.label).color(theme::PAPER), 0.0);
                    if held.is_some() {
                        egui::Frame::new()
                            .fill(theme::VERMILION)
                            .corner_radius(3)
                            .inner_margin(egui::Margin::symmetric(6, 1))
                            .show(ui, |ui| {
                                ui.label(RichText::new("held").small().strong().color(theme::INK));
                            });
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let pencil = egui::Button::new(RichText::new("…").small().color(if editing {
                            theme::PAPER
                        } else {
                            theme::FAINT
                        }))
                        .frame(false);
                        if ui.add(pencil).on_hover_text("Write the options out").clicked() {
                            row.editing_choice = (!editing).then_some(knob.at);
                        }
                    });
                });
                if !editing {
                    if let Some(k) = segmented(ui, knob.at, options, held, can_hold) {
                        row.edits.extend(match held == Some(k) {
                            true => draws::release(options),
                            false => draws::hold(options, k),
                        });
                        row.selected = Some(knob.at);
                    }
                    return;
                }
                // Written out: the options as they stand in the code.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                    for (k, option) in options.iter().enumerate() {
                        let mine = row.editing.as_ref().is_some_and(|e| e.knob == knob.at && e.option == k);
                        let mut text = match &row.editing {
                            Some(e) if mine => e.text.clone(),
                            _ => option.text.clone(),
                        };
                        let width = (text.chars().count() as f32 * 7.6 + 14.0).clamp(34.0, 180.0);
                        let field = egui::TextEdit::singleline(&mut text)
                            .font(FontId::monospace(12.0))
                            .text_color(theme::AMBER)
                            .desired_width(width)
                            .margin(egui::Margin::symmetric(6, 3));
                        let shown = ui.push_id((knob.at, k), |ui| ui.add(field)).inner;
                        if shown.changed() {
                            row.editing = Some(Editing { knob: knob.at, option: k, text: text.clone() });
                        }
                        // The work is rewritten once the option is finished,
                        // so that half-typed code never reaches it.
                        if mine && shown.lost_focus() {
                            if let Some(Editing { text, .. }) = row.editing.take() {
                                if text != option.text && !text.trim().is_empty() {
                                    row.edits.push(Edit { at: option.at.clone(), text });
                                }
                            }
                        }
                        let can_remove = body.is_some() && options.len() > 1;
                        let cross = egui::Button::new(RichText::new("×").small().color(theme::FAINT)).frame(false);
                        if ui.add_enabled(can_remove, cross).on_hover_text("Take this one out").clicked() {
                            row.edits.extend(remove_option(source, options, k));
                        }
                    }
                    if let Some(body) = body {
                        if ui.button(RichText::new("+").small()).on_hover_text("One more, copied from the last").clicked() {
                            row.edits.extend(add_option(source, options, body));
                        }
                    }
                });
            }
        }
    });
    if take_me_there {
        row.selected = Some(knob.at);
        row.reveal = Some(knob.at..knob.at + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlighting_keeps_every_character() {
        let font = FontId::monospace(13.0);
        for src in [STARTER, "work Été {\n  let a = \"open", "", "~~"] {
            assert_eq!(highlight(src, &font).text, src);
        }
    }

    #[test]
    fn fit_keeps_proportions_inside_the_area() {
        let area = Rect::from_min_size(egui::pos2(10.0, 10.0), Vec2::new(400.0, 200.0));
        let r = fit(Vec2::new(100.0, 100.0), area);
        assert_eq!((r.width(), r.height(), r.center()), (200.0, 200.0, area.center()));
    }

    #[test]
    fn thumbnails_are_small() {
        let p = Preview {
            summary: Default::default(),
            width: 512,
            height: 640,
            full: (1024, 1280),
            rgb: vec![7; 512 * 640 * 3],
            elapsed: Duration::ZERO,
        };
        let t = thumbnail(&p);
        assert_eq!(t.size, [115, 144]);
    }
}
