//! The window: tabs on top, code on the left, the edition in the middle,
//! what it is on the right, and the seed along the bottom.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, ColorImage, Context, FontId, Key, Layout, Modifiers, Rect, RichText, Sense, Stroke, TextureOptions,
    Vec2,
};

use crate::doc::{format_seed, parse_seed, random_seed, Doc, Tab, STARTER};
use crate::engine::{self, Engine, Job, Outcome, Preview};
use crate::theme;

/// How long the text must rest before a preview is drawn.
const DEBOUNCE: Duration = Duration::from_millis(140);
/// Longest side of a history thumbnail, in pixels.
const THUMB: usize = 144;

type Exported = Result<PathBuf, String>;

pub struct App {
    docs: Vec<Doc>,
    tabs: Vec<Tab>,
    active: usize,
    engine: Engine,
    next_id: u64,
    serial: u64,
    exports: (Sender<Exported>, Receiver<Exported>),
    exporting: bool,
    /// A line of news in the top bar, and whether it is bad news.
    notice: Option<(String, bool, Instant)>,
    /// Longest side the viewport can show, in pixels (rounded up, so small
    /// resizes do not redraw).
    side: usize,
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
            exporting: false,
            notice: None,
            side: 512,
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
            source: Arc::from(doc.text.as_str()),
            file: doc.file_name(),
            dir: doc.dir(),
            seed: tab.seed,
            max_side: self.side,
        }
    }

    fn export(&mut self) {
        if self.exporting {
            return;
        }
        let tab = &self.tabs[self.active];
        let Some(dir) =
            rfd::FileDialog::new().set_title("Export this edition to…").set_directory(self.docs[tab.doc].dir()).pick_folder()
        else {
            return;
        };
        let (job, done) = (self.job(tab, 0), self.exports.0.clone());
        self.exporting = true;
        std::thread::spawn(move || {
            let _ = done.send(engine::export_png(&job, &dir));
        });
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
                    let version = tab.asked.map_or(0, |a| a.0);
                    let image = ColorImage::from_rgb([p.width, p.height], &p.rgb);
                    tab.image = Some(ctx.load_texture(format!("edition-{}", tab.id), image, TextureOptions::LINEAR));
                    tab.image_px = (p.width, p.height);
                    tab.full = p.full;
                    tab.failure = None;
                    if reply.last {
                        tab.elapsed = Some(p.elapsed);
                        let thumb =
                            ctx.load_texture(format!("thumb-{}-{:x}", tab.id, tab.seed), thumbnail(&p), TextureOptions::LINEAR);
                        if let Some(shot) = tab.history.get_mut(tab.at) {
                            shot.thumb = Some((thumb, version));
                        }
                    }
                    tab.summary = Some(p.summary);
                }
                Outcome::NoImage(summary) => {
                    tab.summary = Some(summary);
                    tab.image = None;
                    tab.failure = None;
                }
                // The last good image stays on screen, dimmed, while the code is broken.
                Outcome::Failed(f) => tab.failure = Some(f),
            }
        }
        while let Ok(done) = self.exports.1.try_recv() {
            self.exporting = false;
            match done {
                Ok(path) => self.say(format!("Exported {}", path.display()), false),
                Err(e) => self.say(format!("Export failed: {e}"), true),
            }
        }
        if self.notice.as_ref().is_some_and(|n| n.2.elapsed() > Duration::from_secs(8)) {
            self.notice = None;
        }
    }

    /// Asks for a new preview when the text, the seed or the viewport changed.
    fn request(&mut self, ctx: &Context) {
        let tab = &self.tabs[self.active];
        let doc = &self.docs[tab.doc];
        let want = (doc.version, tab.seed, self.side);
        if tab.asked == Some(want) {
            return;
        }
        // Only typing waits: a reroll should feel instant.
        let typing = tab.asked.is_some_and(|a| a.0 != doc.version);
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
                    let label = if self.exporting { "Exporting…" } else { "Export PNG" };
                    let export = egui::Button::new(RichText::new(label).strong().color(theme::INK)).fill(theme::PAPER);
                    let has_image =
                        self.tabs[self.active].summary.as_ref().is_some_and(|s| s.renders.iter().any(|r| r == "image"));
                    if ui
                        .add_enabled(has_image && !self.exporting, export)
                        .on_hover_text("The edition on screen, at the size the work asks for")
                        .clicked()
                    {
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
        let tab = &self.tabs[self.active];
        let doc = &mut self.docs[tab.doc];
        let frame = egui::Frame::new().fill(theme::PANEL);
        egui::SidePanel::left("code").frame(frame).default_width(470.0).width_range(280.0..=760.0).show(ctx, |ui| {
            header(ui, "CODE", |ui| {
                let (text, ink) = match (&tab.failure, tab.busy, tab.elapsed) {
                    (Some(_), _, _) => ("error".to_string(), theme::BLUSH),
                    (None, true, _) => ("drawing…".to_string(), theme::AMBER),
                    (None, false, Some(t)) => (format!("live · {} ms", t.as_millis()), theme::MOSS),
                    _ => (String::new(), theme::MUTED),
                };
                ui.label(RichText::new(text).small().color(ink));
            });

            if let Some(failure) = &tab.failure {
                let frame = egui::Frame::new().fill(Color32::from_rgb(0x2a, 0x1b, 0x16)).inner_margin(12);
                egui::TopBottomPanel::bottom("diagnostic").frame(frame).show_inside(ui, |ui| {
                    egui::ScrollArea::both().max_height(140.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(failure.text.trim_end()).monospace().color(theme::BLUSH)).extend());
                    });
                });
            }

            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let font = FontId::monospace(13.0);
                let row = ui.fonts_mut(|f| f.row_height(&font));
                let lines = doc.text.matches('\n').count() + 1;
                let gutter = 46.0;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let (numbers, _) = ui.allocate_exact_size(Vec2::new(gutter, row * lines as f32 + 24.0), Sense::hover());

                    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
                        ui.painter().layout_job(highlight(text.as_str(), &font))
                    };
                    let out = egui::TextEdit::multiline(&mut doc.text)
                        .frame(false)
                        .desired_width(f32::INFINITY)
                        .desired_rows(lines.max(24))
                        .margin(egui::Margin::symmetric(0, 12))
                        .lock_focus(true)
                        .layouter(&mut layouter)
                        .show(ui);
                    if out.response.changed() {
                        doc.edited();
                    }

                    let top = out.galley_pos.y;
                    let bad = tab.failure.as_ref().and_then(|f| f.line);
                    for n in 1..=lines {
                        let y = top + row * (n - 1) as f32;
                        if !ui.is_rect_visible(Rect::from_min_size(egui::pos2(numbers.left(), y), Vec2::new(gutter, row))) {
                            continue;
                        }
                        let wrong = bad == Some(n);
                        if wrong {
                            let line = Rect::from_min_size(
                                egui::pos2(numbers.left(), y),
                                Vec2::new(ui.clip_rect().width() + gutter, row),
                            );
                            ui.painter().rect_filled(line, 0, Color32::from_rgba_unmultiplied(0xf0, 0x69, 0x4c, 34));
                            ui.painter().rect_filled(Rect::from_min_size(line.min, Vec2::new(2.0, row)), 0, theme::VERMILION);
                        }
                        let ink = if wrong { theme::BLUSH } else { theme::FAINT };
                        ui.painter().text(egui::pos2(numbers.right() - 14.0, y), Align2::RIGHT_TOP, n, font.clone(), ink);
                    }
                });
            });
        });
    }

    fn edition_pane(&mut self, ctx: &Context) {
        let tab = &self.tabs[self.active];
        let frame = egui::Frame::new().fill(theme::PANEL);
        egui::SidePanel::right("edition").frame(frame).default_width(260.0).width_range(200.0..=420.0).show(ctx, |ui| {
            header(ui, "THIS EDITION", |_| {});
            egui::Frame::new().inner_margin(14).show(ui, |ui| {
                egui::Grid::new("facts").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                    let fact = |ui: &mut egui::Ui, name: &str, value: String, ink: Color32| {
                        ui.label(RichText::new(name).color(theme::MUTED));
                        ui.label(RichText::new(value).monospace().color(ink));
                        ui.end_row();
                    };
                    if let Some(s) = &tab.summary {
                        fact(ui, "work", s.name.clone(), theme::PAPER);
                    }
                    fact(ui, "seed", format_seed(tab.seed), theme::BLUSH);
                    if let Some(s) = &tab.summary {
                        fact(ui, "renders", s.renders.join(", "), theme::PAPER);
                        if tab.image.is_some() {
                            fact(ui, "export size", format!("{} × {}", tab.full.0, tab.full.1), theme::PAPER);
                            fact(ui, "preview", format!("{} × {}", tab.image_px.0, tab.image_px.1), theme::TEXT);
                        }
                        if s.attempts > 1 {
                            fact(ui, "attempts", s.attempts.to_string(), theme::TEXT);
                        }
                    }
                    fact(ui, "history", format!("{} of {}", tab.at + 1, tab.history.len()), theme::TEXT);
                });
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(
                    RichText::new("The same seed draws the same edition, here and with the command line.").color(theme::MUTED),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("alea {} --seed {}", self.docs[tab.doc].file_name(), format_seed(tab.seed)))
                        .monospace()
                        .color(theme::TEXT),
                );
            });
        });
    }

    fn viewport(&mut self, ctx: &Context) {
        let tab = &self.tabs[self.active];
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::WELL)).show(ctx, |ui| {
            header(ui, "RENDERS", |ui| {
                if tab.image.is_some() {
                    let zoom = 100.0 * tab.image_px.0 as f32 / tab.full.0.max(1) as f32;
                    ui.label(
                        RichText::new(format!("{} × {} · preview at {zoom:.0} %", tab.full.0, tab.full.1))
                            .small()
                            .monospace()
                            .color(theme::MUTED),
                    );
                }
                if let Some(s) = &tab.summary {
                    for kind in s.renders.iter().rev() {
                        let shown = kind == "image";
                        let (ink, fill) = if shown { (theme::INK, theme::PAPER) } else { (theme::MUTED, theme::WELL) };
                        egui::Frame::new()
                            .fill(fill)
                            .stroke(Stroke::new(1.0_f32, theme::EDGE))
                            .corner_radius(10)
                            .inner_margin(egui::Margin::symmetric(9, 1))
                            .show(ui, |ui| {
                                ui.label(RichText::new(kind).small().strong().color(ink));
                            });
                    }
                }
            });

            let area = ui.available_rect_before_wrap().shrink(28.0);
            // What the viewport can show decides the size of the preview.
            let px = (area.width().min(area.height()) * ctx.pixels_per_point()).max(64.0) as usize;
            self.side = px.div_ceil(128) * 128;

            match (&tab.image, &tab.summary) {
                (Some(image), _) => {
                    let rect = fit(image.size_vec2(), area);
                    let tint = if tab.failure.is_some() { Color32::from_gray(110) } else { Color32::WHITE };
                    ui.painter().rect_filled(rect.expand(1.0), 0, theme::LINE);
                    ui.painter().image(image.id(), rect, UV, tint);
                }
                (None, Some(s)) if tab.failure.is_none() => {
                    let what = s.renders.join(", ");
                    let text = format!("{} renders {what}.\nThe viewport shows `render image` for now.", s.name);
                    ui.painter().text(area.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), theme::MUTED);
                }
                _ => {
                    let text =
                        if tab.failure.is_some() { "Nothing to show until the work reads correctly." } else { "Drawing…" };
                    ui.painter().text(area.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), theme::MUTED);
                }
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.collect(ctx);
        self.shortcuts(ctx);
        self.top_bar(ctx);
        self.seed_strip(ctx);
        self.code_pane(ctx);
        self.edition_pane(ctx);
        self.viewport(ctx);
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
