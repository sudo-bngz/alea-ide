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
use alea::eval::{self, Edition};
use alea::library::{self, Module};
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
    /// The work is fine but has no `render image`.
    NoImage(Summary),
    Failed(Failure),
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
    pub message: String,
    /// 1-based line in the tab's own text; `None` when the error is in a
    /// file it `use`s, or has no position.
    pub line: Option<usize>,
    /// alea's full diagnostic, with the source line and an underline.
    pub text: String,
}

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

fn failure(e: Error, job: &Job) -> Failure {
    let elsewhere = e.origin.as_ref().is_some_and(|(file, _)| *file != job.file);
    Failure { text: e.display(&job.source, &job.file), line: e.span.filter(|_| !elsewhere).map(|s| s.line), message: e.msg }
}

/// Runs `f`, turning a panic inside the engine into an ordinary failure: a
/// work that trips a bug must not take the editor down with it.
fn guarded<T>(job: &Job, f: impl FnOnce() -> Res<T>) -> Result<T, Failure> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(failure(e, job)),
        Err(_) => Err(Failure {
            message: "alea stopped unexpectedly while drawing this work".into(),
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

        // A quick pass first, when it is worth one, then the real size.
        let quick = job.max_side / 4;
        let passes: &[usize] = if quick >= 96 { &[quick, job.max_side] } else { &[job.max_side] };
        for (k, side) in passes.iter().enumerate() {
            let last = k + 1 == passes.len();
            match guarded(&job, || paint(&ed, Some(*side))) {
                Ok(Some(preview)) => send(last, Outcome::Image(preview)),
                Ok(None) => {
                    send(true, Outcome::NoImage(summary(&ed)));
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

/// Writes the edition's image at its full size as `<work>-<seed>.png` in
/// `dir`, named like the command line names it.
pub fn export_png(job: &Job, dir: &Path) -> Result<PathBuf, String> {
    let preview = guarded(job, || {
        let ed = edition(job)?;
        paint(&ed, None)
    })
    .map_err(|f| f.message)?
    .ok_or("this work has no `render image` to export")?;
    let slug = Path::new(&job.file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "work".into());
    let path = dir.join(format!("{slug}-{:016x}.png", job.seed));
    let img = image::RgbImage::from_raw(preview.width as u32, preview.height as u32, preview.rgb)
        .ok_or("the image buffer has the wrong size")?;
    img.save(&path).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROSACE: &str = include_str!("../assets/starter.alea");

    fn job(source: &str, seed: u64) -> Job {
        Job { id: 1, source: Arc::from(source), file: "rosace.alea".into(), dir: PathBuf::from("."), seed, max_side: 96 }
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
        assert!(!f.message.is_empty());
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
    fn export_writes_the_full_size_under_the_seed_name() {
        let dir = std::env::temp_dir().join(format!("alea-ide-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = export_png(&job(ROSACE, 5), &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "rosace-0000000000000005.png");
        let (w, h) = image::image_dimensions(&path).unwrap();
        let ed = edition(&job(ROSACE, 5)).map_err(|e| e.msg).unwrap();
        let full = paint(&ed, Some(16)).map_err(|e| e.msg).unwrap().unwrap().full;
        assert_eq!((w as usize, h as usize), full);
        std::fs::remove_dir_all(&dir).unwrap();
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
