//! alea IDE: a studio for the alea language.
//!
//! `alea-ide [FILE.alea]` opens a work next to its edition. With no file it
//! opens a starter work.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod complete;
mod doc;
mod draws;
mod engine;
mod export;
mod reference;
mod score;
mod sound;
mod steps;
mod theme;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let file = std::env::args().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("alea")
            .with_app_id("alea-ide")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("alea", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, file)))))
}
