use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{error::TrySendError, Sender};

use super::encoder::OpusEncoder;
use super::resample::{PcmBlock, PcmFormat, Resampler};
use super::synthetic::SyntheticSource;
use super::{wanted_session, AudioResult, AudioStats, SessionPacket, SAMPLE_RATE};
use crate::capture::timing::FrameTimer;
use crate::control::SharedControl;
use crate::stats::PIPELINE_STATS;
use crate::supervisor::{generation_is_current, HealthReporter, Stage, StageOutcome};

// A synthetic capture block is one Opus frame, so discontinuity resets
// never discard half a frame and shift the tone/silence boundaries.
const SYNTHETIC_BLOCK_FRAMES: usize = super::FRAME_SAMPLES;
const SYNTHETIC_BLOCK_US: u64 = SYNTHETIC_BLOCK_FRAMES as u64 * 1_000_000 / SAMPLE_RATE as u64;
const FRAME_US: u64 = super::FRAME_SAMPLES as u64 * 1_000_000 / SAMPLE_RATE as u64;

enum Source {
    Synthetic {
        source: SyntheticSource,
        next: Instant,
        timer: FrameTimer,
        first: bool,
    },
    #[cfg(windows)]
    Wasapi(Box<super::wasapi::WasapiSource>),
    #[cfg(test)]
    Scripted(std::collections::VecDeque<PcmBlock>),
}

impl Source {
    fn open() -> AudioResult<Self> {
        let selection = std::env::var("ETERNAL_AUDIO").ok();
        if selection
            .as_deref()
            .is_some_and(|value| value != "synthetic" && value != "wasapi")
        {
            return Err("ETERNAL_AUDIO must be synthetic or wasapi".into());
        }
        #[cfg(windows)]
        if selection.as_deref() != Some("synthetic") {
            return Ok(Self::Wasapi(Box::new(super::wasapi::WasapiSource::new()?)));
        }
        #[cfg(not(windows))]
        if selection.as_deref() == Some("wasapi") {
            return Err("WASAPI requires Windows".into());
        }
        Ok(Self::Synthetic {
            source: SyntheticSource::default(),
            next: Instant::now(),
            timer: FrameTimer::new(),
            first: true,
        })
    }

    fn name(&self) -> &str {
        match self {
            Self::Synthetic { .. } => "Synthetic 1 kHz tone",
            #[cfg(windows)]
            Self::Wasapi(source) => source.name(),
            #[cfg(test)]
            Self::Scripted(_) => "Scripted test source",
        }
    }

    fn read(&mut self) -> AudioResult<Option<PcmBlock>> {
        match self {
            Self::Synthetic {
                source,
                next,
                timer,
                first,
            } => {
                *next += Duration::from_micros(SYNTHETIC_BLOCK_US);
                timer.wait_until(*next);
                let late = next.elapsed() > Duration::from_millis(20);
                if late {
                    *next = Instant::now();
                }
                let samples = source
                    .read(SYNTHETIC_BLOCK_FRAMES)
                    .into_iter()
                    .flat_map(|(l, r)| [l, r])
                    .collect();
                Ok(Some(PcmBlock {
                    format: PcmFormat::STEREO_48K,
                    samples,
                    capture_ts_us: crate::clock::host_now_us().saturating_sub(SYNTHETIC_BLOCK_US),
                    discontinuity: std::mem::take(first) || late,
                }))
            }
            #[cfg(windows)]
            Self::Wasapi(source) => source.read(),
            #[cfg(test)]
            Self::Scripted(blocks) => Ok(blocks.pop_front()),
        }
    }
}

struct CaptureSession {
    id: u32,
    source: Source,
    resampler: Option<Resampler>,
    encoder: OpusEncoder,
    mark_discontinuity: bool,
    next_input_us: Option<u64>,
    started_us: Option<u64>,
}

impl CaptureSession {
    fn new(id: u32) -> AudioResult<Self> {
        Ok(Self {
            id,
            source: Source::open()?,
            resampler: None,
            encoder: OpusEncoder::new(576 - eternal_wire::v2::audio::AUDIO_HEADER_SIZE)?,
            mark_discontinuity: true,
            next_input_us: None,
            started_us: None,
        })
    }

    fn tick(
        &mut self,
        tx: &Sender<SessionPacket>,
        shared: &SharedControl,
        generation: u64,
    ) -> AudioResult<bool> {
        let Some(block) = self.read()? else {
            return Ok(true);
        };
        if !generation_is_current(generation) || !shared.running.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if wanted_session(shared) != Some(self.id) {
            return Ok(true);
        }
        self.send(block, tx)
    }

    fn read(&mut self) -> AudioResult<Option<PcmBlock>> {
        self.started_us
            .get_or_insert_with(crate::clock::host_now_us);
        self.source.read()
    }

    fn send(&mut self, block: PcmBlock, tx: &Sender<SessionPacket>) -> AudioResult<bool> {
        let gap = self
            .next_input_us
            .is_some_and(|expected| block.capture_ts_us.abs_diff(expected) > 100_000);
        if block.discontinuity
            || gap
            || self
                .resampler
                .as_ref()
                .is_none_or(|resampler| resampler.format() != block.format)
        {
            self.resampler = Some(Resampler::new(block.format)?);
            self.encoder.reopen()?;
            self.mark_discontinuity = true;
        }
        self.next_input_us = Some(
            block.capture_ts_us
                + block.samples.len() as u64 / u64::from(block.format.channels) * 1_000_000
                    / u64::from(block.format.rate),
        );
        {
            let mut stats = PIPELINE_STATS.lock();
            stats.audio.device = self.source.name().into();
            stats.audio.status = "Streaming".into();
        }
        let started_us = self.started_us.unwrap_or(0);
        for frame in self.resampler.as_mut().unwrap().push(&block)? {
            // WASAPI buffers from Start() while the session opens its encoder,
            // and an idle engine's first read is silence back to Start().
            // Frames that ended before the first read predate the session and
            // are already near the sender's deadline; numbering them would
            // turn each late one into a gap for the viewer. Whole frames keep
            // the Opus frame grid where the source put it.
            if frame.capture_ts_us + FRAME_US <= started_us {
                continue;
            }
            let mut packet = self.encoder.encode(&frame.samples, frame.capture_ts_us)?;
            packet.discontinuity |= self.mark_discontinuity;
            match tx.try_send(SessionPacket {
                session_id: self.id,
                packet,
            }) {
                Ok(()) => self.mark_discontinuity = false,
                Err(TrySendError::Full(rejected)) => {
                    self.mark_discontinuity = true;
                    let mut stats = PIPELINE_STATS.lock();
                    stats.audio.dropped += 1;
                    stats.audio.capture_queue_dropped += 1;
                    stats.audio.late_seqs.record(rejected.packet.seq);
                }
                Err(TrySendError::Closed(_)) => return Ok(false),
            }
        }
        Ok(true)
    }
}

/// An endpoint is opened only after both peers opt in. A failed capture
/// attempt stays disabled for that session; reconnecting or toggling the
/// host audio setting off and on retries without restarting video.
pub fn run_audio_stage(
    tx: Sender<SessionPacket>,
    shared: SharedControl,
    generation: u64,
    reporter: HealthReporter,
) {
    let mut active: Option<CaptureSession> = None;
    let mut failed_session = None;
    let mut next_log = Instant::now() + Duration::from_secs(1);
    while shared.running.load(Ordering::SeqCst)
        && generation_is_current(generation)
        && !tx.is_closed()
    {
        let wanted = wanted_session(&shared);
        if wanted.is_none() {
            active = None;
            if !shared.audio_enabled.load(Ordering::SeqCst) {
                failed_session = None;
            }
            let mut stats = PIPELINE_STATS.lock();
            stats
                .audio
                .stopped(if shared.audio_enabled.load(Ordering::SeqCst) {
                    "Waiting for client"
                } else {
                    "Off"
                });
            if failed_session.is_none() {
                stats.audio.error = None;
            }
            drop(stats);
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let session_id = wanted.unwrap();
        if failed_session == Some(session_id) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let result = (|| -> AudioResult<bool> {
            if active
                .as_ref()
                .is_none_or(|session| session.id != session_id)
            {
                active = None;
                let session = CaptureSession::new(session_id)?;
                if !generation_is_current(generation) || !shared.running.load(Ordering::SeqCst) {
                    return Ok(false);
                }
                PIPELINE_STATS.lock().audio = AudioStats::started(session.source.name());
                tracing::info!(
                    session_id,
                    device = session.source.name(),
                    bitrate = super::BITRATE,
                    "PC audio session started (Opus, 48 kHz stereo, 20 ms)"
                );
                active = Some(session);
            }
            active.as_mut().unwrap().tick(&tx, &shared, generation)
        })();
        if !generation_is_current(generation) || !shared.running.load(Ordering::SeqCst) {
            break;
        }
        match result {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                active = None;
                failed_session = Some(session_id);
                let reason = error.to_string();
                let mut stats = PIPELINE_STATS.lock();
                stats.audio.stopped("Unavailable");
                stats.audio.error = Some(reason.clone());
                drop(stats);
                reporter.stage_exited(Stage::Audio, StageOutcome::Failed(reason));
            }
        }
        if Instant::now() >= next_log {
            next_log = Instant::now() + Duration::from_secs(1);
            let stats = PIPELINE_STATS.lock().audio.clone();
            tracing::info!(device = %stats.device, kbps = stats.kbps,
                packets_per_sec = stats.packets_per_sec, quiet_packets = stats.quiet_packets,
                dropped = stats.dropped, capture_queue_dropped = stats.capture_queue_dropped,
                stale_dropped = stats.stale_dropped, oversized_dropped = stats.oversized_dropped,
                send_failed = stats.send_failed, late_seqs = %stats.late_seqs,
                failed_seqs = %stats.failed_seqs, "Audio stream stats");
        }
    }
    drop(active);
    if generation_is_current(generation) {
        PIPELINE_STATS.lock().audio.stopped("Stopped");
    }
    reporter.stage_exited(Stage::Audio, StageOutcome::Completed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_silence_keeps_opus_alignment_after_a_late_capture() {
        ffmpeg_next::init().unwrap();
        let mut source = SyntheticSource::default();
        let mut resampler = Resampler::new(PcmFormat::STEREO_48K).unwrap();
        let mut quiet = 0;
        for index in 0..96_000 / SYNTHETIC_BLOCK_FRAMES {
            // A late read recreates swresample. A half-Opus-frame remainder
            // here used to shift the 100 ms quiet window by 10 ms forever.
            if index == 3 {
                resampler = Resampler::new(PcmFormat::STEREO_48K).unwrap();
            }
            let samples = source
                .read(SYNTHETIC_BLOCK_FRAMES)
                .into_iter()
                .flat_map(|(l, r)| [l, r])
                .collect();
            for frame in resampler
                .push(&PcmBlock {
                    format: PcmFormat::STEREO_48K,
                    samples,
                    capture_ts_us: index as u64 * SYNTHETIC_BLOCK_FRAMES as u64 * 1_000_000
                        / u64::from(SAMPLE_RATE),
                    discontinuity: index == 3,
                })
                .unwrap()
            {
                if frame.samples.iter().all(|&(l, r)| l == 0.0 && r == 0.0) {
                    quiet += 1;
                }
            }
        }
        assert_eq!(
            quiet, 5,
            "100 ms silence must retain five complete Opus frames"
        );
    }

    #[test]
    fn loopback_backlog_from_before_the_session_is_not_sent_stale() {
        ffmpeg_next::init().unwrap();
        // WASAPI buffers from Start() while the session opens its encoder. A
        // 0.3.1 host on an i7-5500U took 105 ms, and its first six packets
        // missed the sender's deadline. Those packets carried sequence
        // numbers, so the iPad counted each one as lost.
        //
        // Timestamps are explicit, so the test needs no particular speed:
        // a packet goes out once its audio exists and the session has
        // started, so its age at send is max(start, end of its audio) - capture.
        const START: u64 = 10_000_000;
        let mut tone = SyntheticSource::default();
        let mut block = |capture_ts_us: u64, frames: usize| PcmBlock {
            format: PcmFormat::STEREO_48K,
            samples: tone
                .read(frames)
                .into_iter()
                .flat_map(|(l, r)| [l, r])
                .collect(),
            capture_ts_us,
            discontinuity: false,
        };
        let cases = [
            // Sixteen 10 ms engine periods published before the first read.
            (
                "packet backlog",
                (0..36u64)
                    .map(|index| block(START - 160_000 + index * 10_000, 480))
                    .collect::<std::collections::VecDeque<_>>(),
            ),
            // An idle engine: the first read fills 100 ms since Start() with
            // silence, most of it from before the session.
            (
                "idle silence",
                std::iter::once(block(START - 95_000, 4800))
                    .chain((0..20u64).map(|index| block(START + 5_000 + index * 10_000, 480)))
                    .collect(),
            ),
        ];
        // The first read starts the session on the host clock.
        let before = crate::clock::host_now_us();
        let mut unstarted = CaptureSession {
            id: 1,
            source: Source::Scripted(Default::default()),
            resampler: None,
            encoder: OpusEncoder::new(1000).unwrap(),
            mark_discontinuity: true,
            next_input_us: None,
            started_us: None,
        };
        assert!(unstarted.read().unwrap().is_none());
        assert!(unstarted.started_us.is_some_and(|at| at >= before));
        for (name, blocks) in cases {
            let reads = blocks.len();
            let mut session = CaptureSession {
                id: 1,
                source: Source::Scripted(blocks),
                resampler: None,
                encoder: OpusEncoder::new(1000).unwrap(),
                mark_discontinuity: true,
                next_input_us: None,
                started_us: Some(START),
            };
            let (tx, mut rx) = tokio::sync::mpsc::channel(super::super::QUEUE_CAPACITY);
            let mut sent = Vec::new();
            for _ in 0..reads {
                if let Some(block) = session.read().unwrap() {
                    assert!(session.send(block, &tx).unwrap());
                }
                while let Ok(queued) = rx.try_recv() {
                    sent.push(queued.packet);
                }
            }
            assert!(sent.len() >= 8, "{name}: live audio was not sent");
            let stale: Vec<_> = sent
                .iter()
                .map(|packet| {
                    let send_us = START.max(packet.capture_ts_us + FRAME_US);
                    (packet.seq, send_us - packet.capture_ts_us)
                })
                .filter(|&(_, age)| age > super::super::SEND_DEADLINE_US)
                .collect();
            assert!(
                stale.is_empty(),
                "{name}: packets past the send deadline: {stale:?}"
            );
            let first = &sent[0];
            assert_eq!((first.seq, first.discontinuity), (0, true), "{name}");
            assert!(
                first.capture_ts_us + FRAME_US > START,
                "{name}: the first packet replays audio captured {} us before the session",
                START - first.capture_ts_us
            );
        }
    }

    /// Native check of the same startup path with the PC's real loopback.
    /// The delay stands in for the encoder open that follows WASAPI Start().
    /// Inaudible: it only reads the mix. Something must be rendering (even
    /// silence) for WASAPI to publish packets instead of the idle fallback.
    #[cfg(windows)]
    #[test]
    #[ignore = "opens the default output for loopback; run natively"]
    fn wasapi_startup_backlog_probe() {
        ffmpeg_next::init().unwrap();
        let delays = std::env::var("EM_PROBE_DELAYS_MS").unwrap_or_else(|_| "0,50,105,200".into());
        let trials: usize =
            std::env::var("EM_PROBE_TRIALS").map_or(5, |value| value.parse().unwrap());
        let mut late_starts = 0;
        for delay in delays.split(',').map(|value| value.parse::<u64>().unwrap()) {
            for trial in 0..trials {
                let source =
                    Source::Wasapi(Box::new(super::super::wasapi::WasapiSource::new().unwrap()));
                std::thread::sleep(Duration::from_millis(delay));
                let mut session = CaptureSession {
                    id: 1,
                    source,
                    resampler: None,
                    encoder: OpusEncoder::new(1000).unwrap(),
                    mark_discontinuity: true,
                    next_input_us: None,
                    started_us: None,
                };
                let (tx, mut rx) = tokio::sync::mpsc::channel(super::super::QUEUE_CAPACITY);
                let (mut sent, mut stale, mut first_age, mut blocks) = (0, 0, None, 0);
                let until = Instant::now() + Duration::from_secs(1);
                while Instant::now() < until {
                    if let Some(block) = session.read().unwrap() {
                        blocks += 1;
                        session.send(block, &tx).unwrap();
                    }
                    while let Ok(queued) = rx.try_recv() {
                        let age =
                            crate::clock::host_now_us().saturating_sub(queued.packet.capture_ts_us);
                        first_age.get_or_insert(age);
                        sent += 1;
                        stale += usize::from(age > super::super::SEND_DEADLINE_US);
                    }
                }
                println!("PROBE delay_ms={delay} trial={trial} blocks={blocks} sent={sent} stale={stale} first_age_us={}", first_age.unwrap_or(0));
                late_starts += usize::from(stale > 0);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        assert_eq!(late_starts, 0, "starts that sent audio past the deadline");
    }
}
