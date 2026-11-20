//! The bridge to alea: turn the text of a work and a seed into pixels.
//!
//! Everything here runs off the UI thread. [`Engine`] owns one worker that
//! always draws the latest request and drops the ones it was too late for,
//! so typing never queues up stale previews. A preview comes in two passes,
//! a quick small one and then one at the size of the viewport.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alea::error::{Error, Res};
use alea::eval::{self, Edition, Render};
use alea::library::{self, Module};
use alea::media::Visual;
use alea::{lexer, media, parser};

/// What to draw: one work, one seed.
#[derive(Clone)]
pub struct Job {
    /// Echoed back in the [`Reply`], so the UI can tell which tab asked.
    pub id: u64,
    pub source: Arc<str>,
    /// The file name shown in error messages.
    pub file: String,
    /// The folder `use "other.alea"` and picture paths are relative to.
    pub dir: PathBuf,
    pub seed: u64,
    /// Longest side of the preview, in pixels.
    pub max_side: usize,
    /// Which of the work's renders the viewport is showing.
    pub shown: String,
}

pub struct Reply {
    pub id: u64,
    pub seed: u64,
    /// False for the quick first pass: a sharper one follows.
    pub last: bool,
    pub outcome: Outcome,
}

pub enum Outcome {
    Image(Preview),
    /// The shape of a loop. Its frames follow, one reply each.
    Clip(Clip),
    Frame(Frame),
    /// What the work plays, as lanes of notes.
    Music(Summary, crate::score::Music),
    /// The work is fine, but this is not something the viewport can show.
    Nothing(Summary, String),
    Failed(Failure),
}

/// A `render video` at preview size: what it is, before its frames arrive.
pub struct Clip {
    pub summary: Summary,
    pub width: usize,
    pub height: usize,
    /// The size the work asks for, which is what an export writes.
    pub full: (usize, usize),
    /// Frames per second of the preview, which may be gentler than the
    /// work's own.
    pub fps: f64,
    /// The rate the work asks for, which an export starts from.
    pub asked_fps: f64,
    pub frames: usize,
    pub duration: f64,
}

pub struct Frame {
    pub at: usize,
    pub rgb: Vec<u8>,
}

/// What an edition is, apart from its pixels.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub name: String,
    /// The kinds of its `render` lines, in order: `image`, `sound`…
    pub renders: Vec<String>,
    /// How many draws it took to satisfy the work's constraints.
    pub attempts: u32,
}

pub struct Preview {
    pub summary: Summary,
    pub width: usize,
    pub height: usize,
    /// The size the work asks for, which is what an export writes.
    pub full: (usize, usize),
    pub rgb: Vec<u8>,
    pub elapsed: Duration,
}

#[derive(Clone, Debug)]
pub struct Failure {
    /// 1-based line in the tab's own text; `None` when the error is in a
    /// file it `use`s, or has no position.
    pub line: Option<usize>,
    /// alea's full diagnostic, with the source line and an underline.
    pub text: String,
}

/// A loop is previewed gently: a long one is not worth a frame of memory
/// for every frame of video, and a still picture of it says nothing.
const PREVIEW_FPS: f64 = 15.0;
const PREVIEW_FRAMES: usize = 72;
/// A frame of video is a whole field sampled, so the preview is smaller
/// than a still.
const PREVIEW_SIDE: usize = 384;

/// Parses `source` as the work of `file`, loading what it `use`s from disk.
///
/// alea's own loader reads the main file from disk too; an editor has to
/// compile the text on screen, saved or not.
pub fn compile(source: &Arc<str>, file: &str, dir: &Path) -> Res<Arc<Module>> {
    let at = |e: Error| e.in_file(file, source);
    let program = parser::parse(lexer::lex(source).map_err(at)?).map_err(at)?;
    let mut deps = HashMap::new();
    for u in &program.uses {
        let path = dir.join(&u.path);
        let module = library::load(&path.to_string_lossy()).map_err(|e| match e.origin {
            // An error inside the used file already says where it is.
            Some(_) => e,
            None => at(Error::new(format!("cannot open `{}`: {}", u.path, e.msg), u.span)),
        })?;
        deps.insert(module.work.name.clone(), module);
    }
    Ok(Arc::new(Module { work: program.work, file: file.to_string(), src: source.clone(), dir: dir.to_path_buf(), deps }))
}

/// Draws edition `seed` of the work.
pub fn edition(job: &Job) -> Res<Edition> {
    let module = compile(&job.source, &job.file, &job.dir)?;
    let ed = eval::resolve(&module, job.seed)?;
    media::validate(&ed)?;
    Ok(ed)
}

fn summary(ed: &Edition) -> Summary {
    Summary { name: ed.name.clone(), renders: ed.renders.iter().map(|r| r.kind.clone()).collect(), attempts: ed.attempts }
}

/// Paints the first `render image` of an edition, no larger than `max_side`
/// on its longest side (`None`: at the size the work asks for).
pub fn paint(ed: &Edition, max_side: Option<usize>) -> Res<Option<Preview>> {
    let Some(render) = ed.renders.iter().find(|r| r.kind == "image") else {
        return Ok(None);
    };
    let started = Instant::now();
    let spec = media::image_spec(render)?;
    let longest = spec.w.max(spec.h);
    let scale = max_side.map_or(1.0, |m| (m as f64 / longest as f64).min(1.0));
    let width = ((spec.w as f64 * scale).round() as usize).max(16);
    let height = ((spec.h as f64 * scale).round() as usize).max(16);
    let rgb = render.visual("the field to paint")?.rgb(width, height, spec.at, ed.seed, &spec.stops, spec.range);
    Ok(Some(Preview { summary: summary(ed), width, height, full: (spec.w, spec.h), rgb, elapsed: started.elapsed() }))
}

/// The first render of this kind in the edition.
fn render_of<'a>(ed: &'a Edition, kind: &str) -> Option<&'a Render> {
    ed.renders.iter().find(|r| r.kind == kind)
}

/// How a loop is previewed: its size, how many frames, and how fast.
fn clip_plan(spec: &media::VideoSpec, max_side: usize) -> (usize, usize, f64, usize) {
    let side = max_side.min(PREVIEW_SIDE);
    let scale = (side as f64 / spec.w.max(spec.h) as f64).min(1.0);
    let width = ((spec.w as f64 * scale).round() as usize).max(16);
    let height = ((spec.h as f64 * scale).round() as usize).max(16);
    let fps = (spec.fps as f64).min(PREVIEW_FPS);
    let frames = ((spec.duration * fps).round() as usize).clamp(1, PREVIEW_FRAMES);
    // Keep the loop the length the work asks for, however few frames it got.
    (width, height, frames as f64 / spec.duration, frames)
}

fn failure(e: Error, job: &Job) -> Failure {
    let elsewhere = e.origin.as_ref().is_some_and(|(file, _)| *file != job.file);
    Failure { text: e.display(&job.source, &job.file), line: e.span.filter(|_| !elsewhere).map(|s| s.line) }
}

/// Runs `f`, turning a panic inside the engine into an ordinary failure: a
/// work that trips a bug must not take the editor down with it.
fn guarded<T>(job: &Job, f: impl FnOnce() -> Res<T>) -> Result<T, Failure> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(failure(e, job)),
        Err(_) => Err(Failure {
            line: None,
            text: "alea stopped unexpectedly while drawing this work (this is a bug in the engine, not in your work)".into(),
        }),
    }
}

/// The preview worker.
pub struct Engine {
    jobs: Sender<Job>,
    pub replies: Receiver<Reply>,
}

impl Engine {
    /// `wake` is called from the worker each time a reply is ready.
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, replies) = mpsc::channel::<Reply>();
        std::thread::Builder::new()
            .name("alea-preview".into())
            .spawn(move || worker(inbox, outbox, wake))
            .expect("starting the preview thread");
        Engine { jobs, replies }
    }

    pub fn draw(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// The newest job waiting, if any; `Err` once the UI is gone.
fn newest(inbox: &Receiver<Job>) -> Result<Option<Job>, ()> {
    let mut latest = None;
    loop {
        match inbox.try_recv() {
            Ok(job) => latest = Some(job),
            Err(TryRecvError::Empty) => return Ok(latest),
            Err(TryRecvError::Disconnected) => return Err(()),
        }
    }
}

fn worker(inbox: Receiver<Job>, outbox: Sender<Reply>, wake: impl Fn()) {
    let mut next: Option<Job> = None;
    loop {
        let mut job = match next.take() {
            Some(job) => job,
            None => match inbox.recv() {
                Ok(job) => job,
                Err(_) => return,
            },
        };
        match newest(&inbox) {
            Ok(Some(newer)) => job = newer,
            Ok(None) => {}
            Err(()) => return,
        }

        let send = |last: bool, outcome: Outcome| {
            let _ = outbox.send(Reply { id: job.id, seed: job.seed, last, outcome });
            wake();
        };

        let ed = match guarded(&job, || edition(&job)) {
            Ok(ed) => ed,
            Err(f) => {
                send(true, Outcome::Failed(f));
                continue;
            }
        };
        let summary = summary(&ed);

        if job.shown == "video" {
            match clip(&job, &ed, &summary, &inbox, &send) {
                Ok(Some(newer)) => next = Some(newer),
                Ok(None) => {}
                Err(()) => return,
            }
            continue;
        }
        if job.shown == "sound" || job.shown == "midi" {
            match guarded(&job, || Ok(media::live_score(&ed).transpose()?.map(|score| crate::score::read(&score)))) {
                Ok(Some(music)) if !music.is_empty() => send(true, Outcome::Music(summary, music)),
                Ok(_) => {
                    let what = "This work's sound is made of fields rather than notes, so there is nothing to lay out.";
                    send(true, Outcome::Nothing(summary, what.into()));
                }
                Err(f) => send(true, Outcome::Failed(f)),
            }
            continue;
        }
        if job.shown != "image" {
            let what = format!("The viewport shows pictures, loops and music for now, not `{}`.", job.shown);
            send(true, Outcome::Nothing(summary, what));
            continue;
        }

        // A quick pass first, when it is worth one, then the real size.
        let quick = job.max_side / 4;
        let passes: &[usize] = if quick >= 96 { &[quick, job.max_side] } else { &[job.max_side] };
        for (k, side) in passes.iter().enumerate() {
            let last = k + 1 == passes.len();
            match guarded(&job, || paint(&ed, Some(*side))) {
                Ok(Some(preview)) => send(last, Outcome::Image(preview)),
                Ok(None) => {
                    send(true, Outcome::Nothing(summary.clone(), "This work has no `render image`.".into()));
                    break;
                }
                Err(f) => {
                    send(true, Outcome::Failed(f));
                    break;
                }
            }
            // Something newer arrived while painting: the sharp pass of this
            // one would be thrown away.
            match newest(&inbox) {
                Ok(Some(newer)) => {
                    next = Some(newer);
                    break;
                }
                Ok(None) => {}
                Err(()) => return,
            }
        }
    }
}

/// Draws a loop frame by frame, sending each one off as it is done, so the
/// first picture arrives long before the last. Stops early when a newer job
/// is waiting, and hands it back.
fn clip(
    job: &Job,
    ed: &Edition,
    summary: &Summary,
    inbox: &Receiver<Job>,
    send: &impl Fn(bool, Outcome),
) -> Result<Option<Job>, ()> {
    let Some(render) = render_of(ed, "video") else {
        send(true, Outcome::Nothing(summary.clone(), "This work has no `render video`.".into()));
        return Ok(None);
    };
    let plan = guarded(job, || {
        let spec = media::video_spec(render)?;
        let visual = render.visual("the field to animate")?;
        let (width, height, fps, frames) = clip_plan(&spec, job.max_side);
        // What doesn't move is computed once, and one colour scale serves
        // every frame, so the loop doesn't flicker. A painting looks after
        // its own colours.
        let still = match &visual {
            Visual::Field(f) => Some((
                f.cached_for_frames(width, height, ed.seed, 48),
                spec.range.unwrap_or_else(|| media::animation_bounds(f, width, height, ed.seed)),
            )),
            Visual::Paint(_) => None,
        };
        Ok((spec, width, height, fps, frames, still))
    });
    let (spec, width, height, fps, frames, still) = match plan {
        Ok(plan) => plan,
        Err(f) => {
            send(true, Outcome::Failed(f));
            return Ok(None);
        }
    };
    let clip = Clip {
        summary: summary.clone(),
        width,
        height,
        full: (spec.w, spec.h),
        fps,
        asked_fps: spec.fps as f64,
        frames,
        duration: spec.duration,
    };
    send(false, Outcome::Clip(clip));

    for k in 0..frames {
        let t = k as f64 / frames as f64;
        let frame = guarded(job, || {
            let visual = render.visual("the field to animate")?;
            Ok(match &still {
                Some((field, bounds)) => media::frame_rgb(field, width, height, t, ed.seed, &spec.stops, Some(*bounds)),
                None => visual.rgb(width, height, t, ed.seed, &spec.stops, None),
            })
        });
        match frame {
            Ok(rgb) => send(k + 1 == frames, Outcome::Frame(Frame { at: k, rgb })),
            Err(f) => {
                send(true, Outcome::Failed(f));
                return Ok(None);
            }
        }
        match newest(inbox) {
            Ok(Some(newer)) => return Ok(Some(newer)),
            Ok(None) => {}
            Err(()) => return Err(()),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROSACE: &str = include_str!("../assets/starter.alea");

    const BREATHING: &str = "// a loop\nwork Breathing {\n  let pulse = lfo()\n  let heart = circle(x: 0.5, y: 0.5, size: 0.2 + 0.15 * pulse, softness: 0.02)\n  output heart\n  render video(output, duration: 2, fps: 24, width: 120, palette: \"ember\")\n}\n";

    fn job(source: &str, seed: u64) -> Job {
        Job {
            id: 1,
            source: Arc::from(source),
            file: "rosace.alea".into(),
            dir: PathBuf::from("."),
            seed,
            max_side: 96,
            shown: "image".into(),
        }
    }

    #[test]
    fn the_starter_work_paints() {
        let ed = edition(&job(ROSACE, 5)).map_err(|e| e.msg).unwrap();
        let p = paint(&ed, Some(96)).map_err(|e| e.msg).unwrap().expect("an image");
        assert_eq!(p.summary.name, "Rosace");
        assert_eq!(p.summary.renders, ["image"]);
        assert_eq!((p.width.max(p.height), p.rgb.len()), (96, p.width * p.height * 3));
        assert!(p.full.0 >= p.width);
        // Not a flat fill.
        assert!(p.rgb.chunks(3).any(|px| px != &p.rgb[..3]));
    }

    #[test]
    fn every_work_that_comes_with_the_ide_draws() {
        for (name, _, text) in crate::doc::EXAMPLES {
            let job = Job { file: format!("{name}.alea"), ..job(text, 7) };
            let ed = edition(&job).unwrap_or_else(|e| panic!("{name}: {}", e.msg));
            assert!(!ed.renders.is_empty(), "{name} renders nothing");
            // Whatever it renders, the viewport can show one of them.
            let kinds: Vec<&str> = ed.renders.iter().map(|r| r.kind.as_str()).collect();
            assert!(kinds.iter().any(|k| matches!(*k, "image" | "video")), "{name} renders only {kinds:?}");
            // And the one that plays lays out as lanes, not as an empty roll.
            if kinds.contains(&"sound") {
                let score = media::live_score(&ed).transpose().unwrap_or_else(|e| panic!("{name}: {}", e.msg)).expect("a score");
                let music = crate::score::read(&score);
                assert!(!music.is_empty(), "{name} plays nothing the roll can show");
            }
        }
    }

    #[test]
    fn a_seed_names_an_edition() {
        let pixels = |seed| {
            let ed = edition(&job(ROSACE, seed)).map_err(|e| e.msg).unwrap();
            paint(&ed, Some(64)).map_err(|e| e.msg).unwrap().unwrap().rgb
        };
        assert_eq!(pixels(5), pixels(5));
        assert!((1..12).any(|s| pixels(s) != pixels(5)), "every seed drew the same image");
    }

    #[test]
    fn errors_point_at_a_line() {
        let j = job("work Broken {\n  let a = blob(x: 0.5\n  render image(a)\n}\n", 1);
        let f = guarded(&j, || edition(&j)).unwrap_err();
        assert!(f.line.is_some(), "no line in: {}", f.text);
        assert!(f.text.contains("error"), "{}", f.text);
    }

    #[test]
    fn a_work_without_an_image_is_not_an_error() {
        let j = job("work Hum {\n  render sound(drone(clouds(scale: 3)), duration: 1)\n}\n", 1);
        match guarded(&j, || edition(&j)) {
            Ok(ed) => assert!(paint(&ed, Some(64)).map_err(|e| e.msg).unwrap().is_none()),
            Err(f) => panic!("{}", f.text),
        }
    }

    #[test]
    fn a_loop_is_previewed_smaller_and_slower_than_it_plays() {
        let ed = edition(&job(BREATHING, 3)).map_err(|e| e.msg).unwrap();
        let spec = media::video_spec(render_of(&ed, "video").expect("a video")).map_err(|e| e.msg).unwrap();
        let (w, h, fps, frames) = clip_plan(&spec, 1024);
        assert_eq!((w, h), (120, 120), "smaller than the preview's limit already");
        assert_eq!(frames, 30, "two seconds at fifteen frames");
        assert!((frames as f64 / fps - spec.duration).abs() < 1e-9, "the loop keeps its length");

        // A long loop is not given a frame for every frame of video.
        let long = media::VideoSpec { duration: 30.0, ..spec };
        let (_, _, fps, frames) = clip_plan(&long, 1024);
        assert_eq!(frames, PREVIEW_FRAMES);
        assert!((frames as f64 / fps - 30.0).abs() < 1e-9);
    }

    #[test]
    fn a_loop_comes_back_frame_by_frame() {
        let engine = Engine::new(|| {});
        engine.draw(Job { shown: "video".into(), ..job(BREATHING, 3) });
        let (mut shape, mut frames) = (None, 0);
        while let Ok(reply) = engine.replies.recv_timeout(Duration::from_secs(120)) {
            match reply.outcome {
                Outcome::Clip(clip) => shape = Some(clip),
                Outcome::Frame(frame) => {
                    let clip = shape.as_ref().expect("the shape of the loop comes first");
                    assert_eq!(frame.rgb.len(), clip.width * clip.height * 3);
                    assert_eq!(frame.at, frames);
                    frames += 1;
                }
                Outcome::Failed(f) => panic!("{}", f.text),
                _ => panic!("a video job answered with something else"),
            }
            if reply.last {
                break;
            }
        }
        assert_eq!(frames, shape.expect("a clip").frames);
    }

    #[test]
    fn a_work_without_a_loop_says_so_rather_than_failing() {
        let engine = Engine::new(|| {});
        engine.draw(Job { shown: "video".into(), ..job(ROSACE, 5) });
        let reply = engine.replies.recv_timeout(Duration::from_secs(60)).unwrap();
        match reply.outcome {
            Outcome::Nothing(_, what) => assert!(what.contains("render video"), "{what}"),
            _ => panic!("expected a plain answer"),
        }
    }

    #[test]
    fn the_worker_answers_with_the_latest_job() {
        let engine = Engine::new(|| {});
        for seed in 1..=4 {
            engine.draw(Job { id: seed, ..job(ROSACE, seed) });
        }
        let mut last_id = 0;
        while let Ok(reply) = engine.replies.recv_timeout(Duration::from_secs(60)) {
            assert!(matches!(reply.outcome, Outcome::Image(_)));
            last_id = reply.id;
            if reply.id == 4 && reply.last {
                break;
            }
        }
        assert_eq!(last_id, 4);
    }
}
