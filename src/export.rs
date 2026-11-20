//! Writing editions out.
//!
//! Every render alea has can be written from here. Pictures, loops, meshes
//! and MIDI are written by this crate, so nothing has to be installed; an
//! MP4 is handed to ffmpeg, as the command line does, and is offered only
//! when ffmpeg is on the machine.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use alea::error::{Error, Res};
use alea::eval::{Edition, Render};
use alea::media::{self, Visual};

use crate::engine::{edition, Job};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Mp4,
    Wav,
    Mid,
    Stl,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Gif => "GIF",
            Format::Mp4 => "MP4",
            Format::Wav => "WAV",
            Format::Mid => "MIDI",
            Format::Stl => "STL",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Gif => "gif",
            Format::Mp4 => "mp4",
            Format::Wav => "wav",
            Format::Mid => "mid",
            Format::Stl => "stl",
        }
    }

    /// What a render of this kind can be written as, best first.
    pub fn of(kind: &str) -> &'static [Format] {
        match kind {
            "image" => &[Format::Png, Format::Jpeg],
            "video" => &[Format::Gif, Format::Mp4],
            "sound" => &[Format::Wav],
            "midi" => &[Format::Mid],
            "sculpture" => &[Format::Stl],
            _ => &[],
        }
    }

    /// Whether it takes a size.
    pub fn sized(self) -> bool {
        matches!(self, Format::Png | Format::Jpeg | Format::Gif | Format::Mp4)
    }

    /// Whether it runs at a number of frames a second.
    pub fn moving(self) -> bool {
        matches!(self, Format::Gif | Format::Mp4)
    }

    pub fn needs_ffmpeg(self) -> bool {
        self == Format::Mp4
    }
}

/// Whether ffmpeg is on this machine. Looked up once.
pub fn has_ffmpeg() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok())
}

/// What to write, and how.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Which of the work's renders.
    pub kind: String,
    pub format: Format,
    pub width: usize,
    pub height: usize,
    /// Whether changing one side changes the other.
    pub keep_ratio: bool,
    pub fps: f64,
    /// For JPEG, 1 to 100.
    pub quality: u8,
    pub folder: PathBuf,
}

impl Options {
    /// Where the dialog starts: the size and rate the work asks for, in the
    /// first format its render can be written as.
    pub fn new(kind: &str, size: (usize, usize), fps: f64, folder: PathBuf) -> Option<Options> {
        let format = *Format::of(kind).first()?;
        Some(Options {
            kind: kind.to_string(),
            format,
            width: size.0,
            height: size.1,
            keep_ratio: true,
            fps,
            quality: 92,
            folder,
        })
    }

    /// The proportions to keep when one side is changed.
    pub fn ratio(&self) -> f64 {
        match self.width {
            0 => 1.0,
            w => self.height as f64 / w as f64,
        }
    }

    /// How many frames a loop will have, with these options.
    pub fn frames(&self, duration: f64) -> usize {
        ((duration * self.fps).round() as usize).max(1)
    }

    /// What one edition will be called.
    pub fn file_name(&self, slug: &str, seed: u64) -> String {
        format!("{slug}-{seed:016x}.{}", self.format.ext())
    }
}

/// How far along a run is, for the line under the dialog.
pub struct Progress {
    pub edition: usize,
    pub editions: usize,
    pub what: String,
    /// 0 to 1 within this edition.
    pub done: f32,
}

fn render_of<'a>(ed: &'a Edition, kind: &str) -> Res<&'a Render> {
    ed.renders.iter().find(|r| r.kind == kind).ok_or_else(|| Error::bare(format!("this work has no `render {kind}` to export")))
}

/// What the files of this work are called, as the command line names them.
pub fn slug(job: &Job) -> String {
    Path::new(&job.file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "work".into())
}

/// Writes one file per seed. `tell` is called as it goes, so the dialog can
/// say where it has got to.
pub fn run(job: &Job, options: &Options, seeds: &[u64], tell: &dyn Fn(Progress)) -> Result<Vec<PathBuf>, String> {
    if options.format.needs_ffmpeg() && !has_ffmpeg() {
        return Err(
            "an MP4 needs ffmpeg, which is not on this machine — install it from https://ffmpeg.org, or write a GIF".into()
        );
    }
    if !options.folder.is_dir() {
        return Err(format!("{} is not a folder", options.folder.display()));
    }
    let slug = slug(job);
    let mut written = Vec::new();
    for (k, seed) in seeds.iter().enumerate() {
        let job = Job { seed: *seed, ..job.clone() };
        let path = options.folder.join(options.file_name(&slug, *seed));
        let say = |what: &str, done: f32| tell(Progress { edition: k + 1, editions: seeds.len(), what: what.into(), done });
        say("drawing", 0.0);
        let one = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write_one(&job, options, &path, &say)));
        match one {
            Ok(Ok(())) => written.push(path),
            Ok(Err(e)) => return Err(e.msg),
            Err(_) => return Err("alea stopped unexpectedly while writing this edition".into()),
        }
    }
    Ok(written)
}

fn write_one(job: &Job, options: &Options, path: &Path, say: &dyn Fn(&str, f32)) -> Res<()> {
    let ed = edition(job)?;
    let render = render_of(&ed, &options.kind)?;
    match options.format {
        Format::Png | Format::Jpeg => still(&ed, render, options, path),
        Format::Gif => gif(&ed, render, options, path, say),
        Format::Mp4 => mp4(&ed, render, options, path, say),
        Format::Wav => {
            say("mixing", 0.3);
            wav(path, &media::sound_channels(render, ed.seed)?)
        }
        Format::Mid => {
            say("writing", 0.5);
            let score = media::midi_score(render, &ed.name)?;
            std::fs::write(path, alea::midi::smf(&score, &ed.name)).map_err(|e| writing(path, e))
        }
        Format::Stl => {
            say("meshing", 0.3);
            let spec = media::sculpture_spec(render)?;
            let field = render.required_field(0, "the field to sculpt")?;
            let mesh = media::sculpture_mesh(&media::heights(field, &spec, ed.seed), &spec);
            std::fs::write(path, media::stl_bytes(&mesh, ed.seed)).map_err(|e| writing(path, e))
        }
    }
}

fn writing(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::bare(format!("writing {}: {e}", path.display()))
}

fn still(ed: &Edition, render: &Render, options: &Options, path: &Path) -> Res<()> {
    let spec = media::image_spec(render)?;
    let (w, h) = (options.width.max(1), options.height.max(1));
    let rgb = render.visual("the field to paint")?.rgb(w, h, spec.at, ed.seed, &spec.stops, spec.range);
    let img =
        image::RgbImage::from_raw(w as u32, h as u32, rgb).ok_or_else(|| Error::bare("the image buffer has the wrong size"))?;
    let file = std::fs::File::create(path).map_err(|e| writing(path, e))?;
    let mut out = std::io::BufWriter::new(file);
    match options.format {
        Format::Jpeg => image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, options.quality.clamp(1, 100))
            .encode_image(&img)
            .map_err(|e| writing(path, e)),
        _ => img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| writing(path, e)),
    }
}

/// The frames of a loop, one at a time. What does not move is computed once
/// and one colour scale serves every frame, so the loop does not flicker.
fn frames<'a>(
    ed: &'a Edition,
    render: &'a Render,
    spec: &'a media::VideoSpec,
    options: &'a Options,
) -> Res<(usize, impl Fn(usize) -> Res<Vec<u8>> + 'a)> {
    let (w, h) = (options.width.max(1), options.height.max(1));
    let count = options.frames(spec.duration);
    let visual = render.visual("the field to animate")?;
    let still = match &visual {
        Visual::Field(f) => Some((
            f.cached_for_frames(w, h, ed.seed, 48),
            spec.range.unwrap_or_else(|| media::animation_bounds(f, w, h, ed.seed)),
        )),
        Visual::Paint(_) => None,
    };
    let frame = move |k: usize| {
        let t = k as f64 / count as f64;
        let visual = render.visual("the field to animate")?;
        Ok(match &still {
            Some((field, bounds)) => media::frame_rgb(field, w, h, t, ed.seed, &spec.stops, Some(*bounds)),
            None => visual.rgb(w, h, t, ed.seed, &spec.stops, None),
        })
    };
    Ok((count, frame))
}

fn gif(ed: &Edition, render: &Render, options: &Options, path: &Path, say: &dyn Fn(&str, f32)) -> Res<()> {
    let spec = media::video_spec(render)?;
    let (count, frame) = frames(ed, render, &spec, options)?;
    let (w, h) = (options.width.max(1) as u32, options.height.max(1) as u32);
    let file = std::fs::File::create(path).map_err(|e| writing(path, e))?;
    let mut gif = image::codecs::gif::GifEncoder::new_with_speed(std::io::BufWriter::new(file), 10);
    gif.set_repeat(image::codecs::gif::Repeat::Infinite).map_err(|e| writing(path, e))?;
    // A GIF holds each frame for a whole number of hundredths of a second,
    // so its rate is rounded to one the format can keep.
    let hundredths = (100.0 / options.fps.max(1.0)).round().max(2.0) as u32;
    for k in 0..count {
        say(&format!("frame {} of {count}", k + 1), k as f32 / count as f32);
        let rgb = frame(k)?;
        let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
        for px in rgb.chunks(3) {
            rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        let buffer = image::RgbaImage::from_raw(w, h, rgba).ok_or_else(|| Error::bare("the frame buffer has the wrong size"))?;
        let delay = image::Delay::from_numer_denom_ms(hundredths * 10, 1);
        gif.encode_frame(image::Frame::from_parts(buffer, 0, 0, delay)).map_err(|e| writing(path, e))?;
    }
    Ok(())
}

fn mp4(ed: &Edition, render: &Render, options: &Options, path: &Path, say: &dyn Fn(&str, f32)) -> Res<()> {
    let spec = media::video_spec(render)?;
    let (count, frame) = frames(ed, render, &spec, options)?;
    // Most players expect even sides.
    let (w, h) = (options.width.max(16) & !1, options.height.max(16) & !1);
    if (w, h) != (options.width, options.height) {
        return Err(Error::bare("an MP4 needs even sides: round the width and the height down to even numbers"));
    }

    let audio = path.with_extension("audio.tmp.wav");
    if spec.audio {
        say("mixing the sound", 0.0);
        wav(&audio, &media::video_channels(render, &spec, ed.seed)?)?;
    }
    let clean = || {
        let _ = std::fs::remove_file(&audio);
    };

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24"]).args([
        "-s",
        &format!("{w}x{h}"),
        "-r",
        &format!("{:.4}", options.fps),
        "-i",
        "-",
    ]);
    if spec.audio {
        cmd.arg("-i").arg(&audio);
    }
    cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18", "-movflags", "+faststart"]);
    if spec.audio {
        cmd.args(["-c:a", "aac", "-b:a", "192k", "-shortest"]);
    }
    cmd.arg(path).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            clean();
            return Err(Error::bare(format!("ffmpeg would not start: {e}")));
        }
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut broken = false;
    for k in 0..count {
        say(&format!("frame {} of {count}", k + 1), k as f32 / count as f32);
        match frame(k) {
            Ok(rgb) => {
                if stdin.write_all(&rgb).is_err() {
                    // ffmpeg stopped early; its own words are below.
                    broken = true;
                    break;
                }
            }
            Err(e) => {
                drop(stdin);
                let _ = child.wait();
                clean();
                return Err(e);
            }
        }
    }
    drop(stdin);
    say("encoding", 1.0);
    let done = child.wait_with_output().map_err(|e| writing(path, e))?;
    clean();
    if broken || !done.status.success() {
        let log = String::from_utf8_lossy(&done.stderr);
        let tail: Vec<&str> = log.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(Error::bare(format!("ffmpeg failed while writing {}:\n  {}", path.display(), tail.join("\n  "))));
    }
    Ok(())
}

/// 16-bit WAV, mono or stereo, at alea's own rate.
fn wav(path: &Path, channels: &[Vec<f64>]) -> Res<()> {
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate: media::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = hound::WavWriter::create(path, spec).map_err(|e| writing(path, e))?;
    for s in 0..channels.first().map_or(0, Vec::len) {
        for channel in channels {
            out.write_sample((channel[s] * i16::MAX as f64) as i16).map_err(|e| writing(path, e))?;
        }
    }
    out.finalize().map_err(|e| writing(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const ROSACE: &str = include_str!("../assets/starter.alea");
    const MOVING: &str = "work Moving {\n  let pulse = lfo()\n  let heart = circle(x: 0.5, y: 0.5, size: 0.2 + 0.2 * pulse)\n  output heart\n  render video(output, duration: 1, fps: 12, width: 64, palette: \"ember\")\n}\n";
    const HEARD: &str = "work Heard {\n  let m = meter(120, bars: 1)\n  let hit = steps(\"X...X...X...X...\", bars: m)\n  render sound(notes(hit, instrument: \"drum\", tempo: m.tempo), duration: m.length)\n  render midi(notes(hit, instrument: \"drum\", tempo: m.tempo), duration: m.length, tempo: m.tempo)\n}\n";
    const CARVED: &str = "work Carved {\n  render sculpture(clouds(scale: 4), side: 20, resolution: 40)\n}\n";

    fn job(source: &str) -> Job {
        Job {
            id: 1,
            source: Arc::from(source),
            file: "work.alea".into(),
            dir: PathBuf::from("."),
            seed: 5,
            max_side: 64,
            shown: "image".into(),
        }
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alea-ide-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn written(job: &Job, options: &Options, seeds: &[u64]) -> Vec<PathBuf> {
        run(job, options, seeds, &|_| {}).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn each_render_offers_the_formats_it_can_be_written_as() {
        assert_eq!(Format::of("image"), [Format::Png, Format::Jpeg]);
        assert_eq!(Format::of("video"), [Format::Gif, Format::Mp4]);
        assert_eq!(Format::of("sculpture"), [Format::Stl]);
        assert!(Format::of("nonsense").is_empty());
        assert!(Format::Gif.sized() && Format::Gif.moving());
        assert!(!Format::Wav.sized() && !Format::Png.moving());
    }

    #[test]
    fn the_dialog_starts_from_what_the_work_asks_for() {
        let options = Options::new("image", (1024, 1280), 0.0, PathBuf::from(".")).unwrap();
        assert_eq!((options.format, options.width, options.height), (Format::Png, 1024, 1280));
        assert_eq!(options.file_name("rosace", 0x5), "rosace-0000000000000005.png");
        assert!((options.ratio() - 1.25).abs() < 1e-9, "the proportions to keep");

        let options = Options::new("video", (64, 64), 12.0, PathBuf::from(".")).unwrap();
        assert_eq!((options.format, options.fps), (Format::Gif, 12.0));
        assert_eq!(options.frames(1.0), 12);
        assert_eq!(options.file_name("moving", 0x5), "moving-0000000000000005.gif");
        assert!(Options::new("nonsense", (0, 0), 0.0, PathBuf::from(".")).is_none());
    }

    #[test]
    fn a_picture_is_written_at_the_size_asked_for() {
        let dir = folder("png");
        let mut options = Options::new("image", (1024, 1024), 0.0, dir.clone()).unwrap();
        options.width = 320;
        options.height = 400;
        let files = written(&job(ROSACE), &options, &[0x5, 0x2f1]);
        assert_eq!(files.len(), 2, "one file per edition");
        for file in &files {
            assert_eq!(image::image_dimensions(file).unwrap(), (320, 400));
        }
        assert!(files[0].file_name().unwrap().to_string_lossy().contains("0000000000000005"));

        options.format = Format::Jpeg;
        options.quality = 70;
        let small = written(&job(ROSACE), &options, &[0x5]);
        assert_eq!(small[0].extension().unwrap(), "jpg");
        assert_eq!(image::image_dimensions(&small[0]).unwrap(), (320, 400));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_loop_is_written_as_a_gif_of_the_frames_asked_for() {
        let dir = folder("gif");
        let mut options = Options::new("video", (64, 64), 12.0, dir.clone()).unwrap();
        options.fps = 6.0;
        let (steps, last) = (std::cell::Cell::new(0), std::cell::Cell::new(0.0));
        let files = run(&job(MOVING), &options, &[0x5], &|p| {
            steps.set(steps.get() + 1);
            last.set(p.done);
            assert_eq!((p.edition, p.editions), (1, 1));
        })
        .unwrap();
        assert!(steps.get() > 6 && last.get() > 0.0, "the dialog is told how far along it is");
        assert_eq!(image::image_dimensions(&files[0]).unwrap(), (64, 64));
        assert!(std::fs::metadata(&files[0]).unwrap().len() > 500);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sound_midi_and_a_mesh_are_written_too() {
        let dir = folder("rest");
        for (source, kind, ext) in [(HEARD, "sound", "wav"), (HEARD, "midi", "mid"), (CARVED, "sculpture", "stl")] {
            let options = Options::new(kind, (0, 0), 0.0, dir.clone()).unwrap();
            let files = written(&job(source), &options, &[0x5]);
            assert_eq!(files[0].extension().unwrap(), ext);
            let size = std::fs::metadata(&files[0]).unwrap().len();
            assert!(size > 100, "{kind} wrote {size} bytes");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_mp4_says_what_it_needs() {
        let dir = folder("mp4");
        let mut options = Options::new("video", (64, 64), 12.0, dir.clone()).unwrap();
        options.format = Format::Mp4;
        match (has_ffmpeg(), run(&job(MOVING), &options, &[0x5], &|_| {})) {
            (true, Ok(files)) => assert!(std::fs::metadata(&files[0]).unwrap().len() > 1000),
            (true, Err(e)) => panic!("{e}"),
            (false, Err(e)) => assert!(e.contains("ffmpeg"), "{e}"),
            (false, Ok(_)) => panic!("an MP4 was written without ffmpeg"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_that_is_not_there_is_said_plainly() {
        let options = Options::new("image", (64, 64), 0.0, PathBuf::from("/not/a/folder")).unwrap();
        let said = run(&job(ROSACE), &options, &[0x5], &|_| {}).unwrap_err();
        assert!(said.contains("not a folder"), "{said}");
    }
}
