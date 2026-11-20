//! Hearing an edition.
//!
//! alea renders a work's sound the same way whether it is going to a file
//! or to the speakers, so what plays here is what Export writes. The
//! samples come from the worker; this keeps them, feeds the audio device
//! and remembers where in the loop it is, which is also where the playhead
//! stands in the roll.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, OutputCallbackInfo, SampleFormat, SizedSample, StreamConfig};

/// The music of one edition, ready to play: samples interleaved by channel,
/// as alea renders them.
#[derive(Clone)]
pub struct Take {
    pub samples: Arc<Vec<f32>>,
    pub channels: usize,
    pub rate: u32,
    pub duration: f64,
}

impl Take {
    /// How many frames (one sample per channel) there are to play.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    pub fn from_channels(channels: &[Vec<f64>], rate: u32) -> Take {
        let frames = channels.first().map_or(0, Vec::len);
        let mut samples = Vec::with_capacity(frames * channels.len());
        for s in 0..frames {
            for channel in channels {
                samples.push(channel[s] as f32);
            }
        }
        Take {
            samples: Arc::new(samples),
            channels: channels.len().max(1),
            rate,
            duration: frames as f64 / f64::from(rate.max(1)),
        }
    }
}

/// What the audio callback reads. The lock is held for the length of one
/// buffer and never while anything slow happens; if the interface is in the
/// middle of swapping a take, the callback plays silence rather than wait.
#[derive(Default)]
struct Deck {
    take: Option<Take>,
    /// Where in the take, counted in source frames.
    at: f64,
}

pub struct Speaker {
    deck: Arc<Mutex<Deck>>,
    playing: Arc<AtomicBool>,
    /// Kept alive for as long as there is sound: dropping it closes the
    /// device.
    stream: Option<cpal::Stream>,
    /// Why there is no sound, if there is none.
    trouble: Option<String>,
}

impl Default for Speaker {
    fn default() -> Self {
        Speaker::new()
    }
}

impl Speaker {
    pub fn new() -> Speaker {
        Speaker {
            deck: Arc::new(Mutex::new(Deck::default())),
            playing: Arc::new(AtomicBool::new(false)),
            stream: None,
            trouble: None,
        }
    }

    /// The music to play. A new edition of the same work keeps the place it
    /// was playing from, so turning a knob or moving a step does not stop
    /// the groove.
    pub fn load(&mut self, take: Take) {
        let mut deck = self.deck.lock().unwrap_or_else(|e| e.into_inner());
        let frames = take.frames() as f64;
        if deck.at >= frames {
            deck.at = 0.0;
        }
        deck.take = Some(take);
    }

    pub fn forget(&mut self) {
        self.hold();
        let mut deck = self.deck.lock().unwrap_or_else(|e| e.into_inner());
        deck.take = None;
        deck.at = 0.0;
    }

    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Where in the music it is, in seconds.
    pub fn at(&self) -> f64 {
        let deck = self.deck.lock().unwrap_or_else(|e| e.into_inner());
        match &deck.take {
            Some(take) => deck.at / f64::from(take.rate.max(1)),
            None => 0.0,
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        let mut deck = self.deck.lock().unwrap_or_else(|e| e.into_inner());
        let Some(take) = &deck.take else { return };
        let frames = take.frames() as f64;
        deck.at = (seconds * f64::from(take.rate.max(1))).clamp(0.0, (frames - 1.0).max(0.0));
    }

    pub fn play(&mut self) {
        if self.stream.is_none() {
            match self.open() {
                Ok(stream) => self.stream = Some(stream),
                Err(why) => {
                    self.trouble = Some(why);
                    return;
                }
            }
        }
        self.playing.store(true, Ordering::Relaxed);
        if let Some(stream) = &self.stream {
            if let Err(e) = stream.play() {
                self.trouble = Some(format!("the sound device stopped: {e}"));
            }
        }
    }

    pub fn hold(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        if let Some(stream) = &self.stream {
            let _ = stream.pause();
        }
    }

    pub fn rewind(&mut self) {
        self.hold();
        let mut deck = self.deck.lock().unwrap_or_else(|e| e.into_inner());
        deck.at = 0.0;
    }

    pub fn trouble(&self) -> Option<&str> {
        self.trouble.as_deref()
    }

    /// Opens the default output. Whatever the device wants — its rate, its
    /// number of channels, its sample format — the take is read to fit.
    fn open(&self) -> Result<cpal::Stream, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("this machine has no sound output")?;
        let supported =
            device.default_output_config().map_err(|e| format!("the sound device would not say what it takes: {e}"))?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let (deck, playing) = (Arc::clone(&self.deck), Arc::clone(&self.playing));
        match format {
            SampleFormat::F32 => stream::<f32>(&device, config, deck, playing),
            SampleFormat::I16 => stream::<i16>(&device, config, deck, playing),
            SampleFormat::I32 => stream::<i32>(&device, config, deck, playing),
            SampleFormat::U16 => stream::<u16>(&device, config, deck, playing),
            SampleFormat::F64 => stream::<f64>(&device, config, deck, playing),
            other => Err(format!("the sound device speaks {other}, which the studio does not")),
        }
    }
}

fn stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: StreamConfig,
    deck: Arc<Mutex<Deck>>,
    playing: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let out_channels = config.channels as usize;
    let out_rate = f64::from(config.sample_rate.max(1));
    let mut mix: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &OutputCallbackInfo| {
                mix.clear();
                mix.resize(data.len(), 0.0);
                if playing.load(Ordering::Relaxed) {
                    // Never wait on the interface: silence is better than a
                    // late buffer, which the device hears as a click.
                    if let Ok(mut deck) = deck.try_lock() {
                        if let Some(take) = deck.take.clone() {
                            let step = f64::from(take.rate.max(1)) / out_rate;
                            deck.at = fill(&mut mix, out_channels, &take, deck.at, step);
                        }
                    }
                }
                for (slot, sample) in data.iter_mut().zip(mix.iter()) {
                    *slot = T::from_sample(*sample);
                }
            },
            |e| eprintln!("sound: {e}"),
            None,
        )
        .map_err(|e| format!("the sound device would not open: {e}"))
}

/// Reads `out` out of the take, from `at`, a source frame every `step`, and
/// says where that left off. The music is a loop, so it wraps rather than
/// ending; a take with fewer channels than the device is spread across it,
/// so a mono work is heard on both sides.
fn fill(out: &mut [f32], out_channels: usize, take: &Take, mut at: f64, step: f64) -> f64 {
    let frames = take.frames();
    if frames == 0 || out_channels == 0 {
        return 0.0;
    }
    let channels = take.channels.max(1);
    for frame in out.chunks_mut(out_channels) {
        let here = at as usize % frames;
        let next = (here + 1) % frames;
        let across = at - at.floor();
        for (c, slot) in frame.iter_mut().enumerate() {
            let channel = c.min(channels - 1);
            let a = take.samples[here * channels + channel];
            let b = take.samples[next * channels + channel];
            *slot = a + (b - a) * across as f32;
        }
        at += step;
        if at >= frames as f64 {
            at -= frames as f64;
        }
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(frames: usize, channels: usize, rate: u32) -> Take {
        let samples = (0..frames * channels).map(|k| k as f32).collect();
        Take { samples: Arc::new(samples), channels, rate, duration: frames as f64 / f64::from(rate) }
    }

    #[test]
    fn samples_come_out_as_they_went_in() {
        let t = take(8, 2, 48_000);
        let mut out = vec![0.0; 8];
        let at = fill(&mut out, 2, &t, 0.0, 1.0);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(at, 4.0);
    }

    #[test]
    fn the_music_is_a_loop() {
        let t = take(4, 1, 48_000);
        let mut out = vec![0.0; 6];
        let at = fill(&mut out, 1, &t, 2.0, 1.0);
        assert_eq!(out, [2.0, 3.0, 0.0, 1.0, 2.0, 3.0]);
        assert_eq!(at, 0.0);
    }

    #[test]
    fn a_device_at_another_rate_reads_between_the_samples() {
        // 44.1k into a 88.2k device: every other frame falls between two.
        let t = take(4, 1, 44_100);
        let mut out = vec![0.0; 4];
        fill(&mut out, 1, &t, 0.0, 0.5);
        assert_eq!(out, [0.0, 0.5, 1.0, 1.5]);
    }

    #[test]
    fn a_mono_work_is_heard_on_both_sides() {
        let t = take(2, 1, 48_000);
        let mut out = vec![0.0; 4];
        fill(&mut out, 2, &t, 0.0, 1.0);
        assert_eq!(out, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn nothing_to_play_is_not_a_crash() {
        let t = Take { samples: Arc::new(Vec::new()), channels: 2, rate: 48_000, duration: 0.0 };
        let mut out = vec![7.0; 4];
        assert_eq!(fill(&mut out, 2, &t, 0.0, 1.0), 0.0);
        assert_eq!(out, [7.0; 4]);
    }

    #[test]
    fn channels_are_interleaved_for_the_device() {
        let take = Take::from_channels(&[vec![0.0, 1.0], vec![-1.0, -2.0]], 48_000);
        assert_eq!(*take.samples, [0.0, -1.0, 1.0, -2.0]);
        assert_eq!((take.channels, take.frames()), (2, 2));
    }

    #[test]
    fn a_take_that_is_still_long_enough_keeps_its_place() {
        let mut speaker = Speaker::new();
        speaker.load(take(100, 2, 48_000));
        speaker.seek(50.0 / 48_000.0);
        speaker.load(take(100, 2, 48_000));
        assert!((speaker.at() - 50.0 / 48_000.0).abs() < 1e-9, "it lost its place");
        // A shorter take starts over rather than playing past its end.
        speaker.load(take(10, 2, 48_000));
        assert_eq!(speaker.at(), 0.0);
    }
}
