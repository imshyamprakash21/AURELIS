use std::{
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use super::{
    channel_converter::AudioChannelConverter,
    decoder_stream::AudioDecoder,
    metadata::AudioMetadata,
    output::AudioOutput,
    resampler::AudioResampler,
    spectrum::{SpectrumShared, SpectrumSnapshot},
    types::AudioSpec,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

/// Why a playback worker stopped producing audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackEndReason {
    /// The decoder reached the end of the track.
    Completed,
    /// Playback was stopped by a caller or shutdown request.
    Stopped,
}

enum WorkerResult {
    Completed,
    Stopped,
    Failed(String),
}

enum WorkerCommand {
    Pause,
    Resume,
    Seek,
    Stop,
    Shutdown,
}

struct PlaybackWorker {
    command_tx: mpsc::Sender<WorkerCommand>,
    result_rx: Mutex<mpsc::Receiver<WorkerResult>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl PlaybackWorker {
    fn start(
        path: String,
        played_frames: Arc<AtomicU64>,
        pending_seek: Arc<Mutex<Option<Duration>>>,
        spectrum: Arc<SpectrumShared>,
        volume: Arc<AtomicU32>,
    ) -> Result<Self, Box<dyn Error>> {
        let (command_tx, command_rx) = mpsc::channel::<WorkerCommand>();

        let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();

        let handle = thread::Builder::new()
            .name("aurelis-playback".to_string())
            .spawn(move || {
                let result = run_playback_pipeline(
                    &path,
                    &command_rx,
                    played_frames,
                    pending_seek,
                    spectrum,
                    volume,
                );

                let worker_result = match result {
                    Ok(WorkerPipelineResult::Completed) => WorkerResult::Completed,

                    Ok(WorkerPipelineResult::Stopped) => WorkerResult::Stopped,

                    Err(error) => WorkerResult::Failed(error.to_string()),
                };

                let _ = result_tx.send(worker_result);
            })?;

        Ok(Self {
            command_tx,
            result_rx: Mutex::new(result_rx),
            handle: Some(handle),
        })
    }

    fn send(&self, command: WorkerCommand) -> Result<(), Box<dyn Error>> {
        self.command_tx
            .send(command)
            .map_err(|_| "Playback worker is no longer running".into())
    }

    fn try_result(&self) -> Option<WorkerResult> {
        let receiver = self.result_rx.lock().ok()?;

        match receiver.try_recv() {
            Ok(result) => Some(result),

            Err(mpsc::TryRecvError::Empty) => None,

            Err(mpsc::TryRecvError::Disconnected) => Some(WorkerResult::Failed(
                "Playback worker disconnected".to_string(),
            )),
        }
    }

    fn wait(&mut self) -> Result<WorkerResult, Box<dyn Error>> {
        let result = {
            let receiver = self
                .result_rx
                .lock()
                .map_err(|_| "Playback worker result lock poisoned")?;

            receiver
                .recv()
                .map_err(|_| "Playback worker exited unexpectedly")?
        };

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }

        Ok(result)
    }
}

impl Drop for PlaybackWorker {
    fn drop(&mut self) {
        let _ = self.command_tx.send(WorkerCommand::Shutdown);

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

enum WorkerPipelineResult {
    Completed,
    Stopped,
}

pub struct PlaybackEngine {
    path: String,

    source_spec: AudioSpec,
    output_spec: AudioSpec,

    metadata: AudioMetadata,

    resampling: bool,
    channel_conversion: bool,

    state: PlaybackState,

    worker: Option<PlaybackWorker>,

    played_frames: Arc<AtomicU64>,

    pending_seek: Arc<Mutex<Option<Duration>>>,

    spectrum: Arc<SpectrumShared>,

    volume: Arc<AtomicU32>,
}

impl PlaybackEngine {
    pub fn open(path: &str) -> Result<Self, Box<dyn Error>> {
        let decoder = AudioDecoder::open(path)?;

        let source_spec = decoder.spec();

        let output_spec = AudioOutput::negotiate_spec(source_spec)?;

        let resampling = source_spec.sample_rate != output_spec.sample_rate;

        let channel_conversion = source_spec.channels != output_spec.channels;

        let metadata = AudioMetadata::from_file(path).unwrap_or_default();

        Ok(Self {
            path: path.to_string(),

            source_spec,
            output_spec,

            metadata,

            resampling,
            channel_conversion,

            state: PlaybackState::Stopped,

            worker: None,

            played_frames: Arc::new(AtomicU64::new(0)),

            pending_seek: Arc::new(Mutex::new(None)),

            spectrum: Arc::new(SpectrumShared::new()),

            volume: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
        })
    }

    pub fn source_spec(&self) -> AudioSpec {
        self.source_spec
    }

    pub fn output_spec(&self) -> AudioSpec {
        self.output_spec
    }

    /// Current position based on complete frames consumed by the output callback.
    pub fn position(&self) -> Duration {
        let frames = self.played_frames.load(Ordering::Relaxed);
        let position = frames_to_duration(frames, self.output_spec.sample_rate);

        self.metadata
            .duration
            .map_or(position, |duration| position.min(duration))
    }

    pub fn metadata(&self) -> &AudioMetadata {
        &self.metadata
    }

    /// Read the latest real-time spectrum derived from output PCM samples.
    pub fn spectrum_snapshot(&self) -> SpectrumSnapshot {
        self.spectrum.snapshot()
    }

    /// Current normalized output volume in the range `0.0..=1.0`.
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    /// Set the output volume without interrupting playback.
    pub fn set_volume(&mut self, volume: f32) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
    }

    pub fn is_resampling(&self) -> bool {
        self.resampling
    }

    pub fn is_channel_conversion(&self) -> bool {
        self.channel_conversion
    }

    pub fn state(&mut self) -> PlaybackState {
        self.poll_worker();

        self.state
    }

    /// Start playback asynchronously.
    ///
    /// The playback worker is created and this
    /// function returns immediately.
    ///
    /// The caller can then use pause(), resume(),
    /// stop(), and poll() while playback continues.
    pub fn play(&mut self) -> Result<(), Box<dyn Error>> {
        if self.worker.is_some() {
            return match self.state {
                PlaybackState::Paused => {
                    self.resume();

                    Ok(())
                }

                PlaybackState::Playing => Ok(()),

                PlaybackState::Stopped => self.start_worker(),
            };
        }

        self.start_worker()
    }

    fn start_worker(&mut self) -> Result<(), Box<dyn Error>> {
        println!("Starting playback...");

        self.played_frames.store(0, Ordering::Relaxed);
        self.spectrum.clear();

        let worker = PlaybackWorker::start(
            self.path.clone(),
            Arc::clone(&self.played_frames),
            Arc::clone(&self.pending_seek),
            Arc::clone(&self.spectrum),
            Arc::clone(&self.volume),
        )?;

        self.worker = Some(worker);

        self.state = PlaybackState::Playing;

        Ok(())
    }

    /// Check whether the worker has finished.
    ///
    /// Returns Some(result) when playback has
    /// completed or stopped.
    pub fn poll(&mut self) -> Option<Result<(), Box<dyn Error>>> {
        self.poll_end().map(|result| {
            result.map(|reason| {
                if reason == PlaybackEndReason::Completed {
                    println!("Playback complete.");
                }
            })
        })
    }

    /// Check whether the worker has finished and preserve the reason.
    ///
    /// This is useful to application code that needs to distinguish natural
    /// end-of-track from a manual stop. The older `poll()` API remains
    /// available for callers that only need completion/error notification.
    pub fn poll_end(&mut self) -> Option<Result<PlaybackEndReason, Box<dyn Error>>> {
        match self.poll_worker() {
            Some(WorkerResult::Completed) => Some(Ok(PlaybackEndReason::Completed)),
            Some(WorkerResult::Stopped) => Some(Ok(PlaybackEndReason::Stopped)),
            Some(WorkerResult::Failed(error)) => Some(Err(error.into())),
            None => None,
        }
    }

    fn poll_worker(&mut self) -> Option<WorkerResult> {
        let result = self.worker.as_ref().and_then(|worker| worker.try_result());

        if result.is_some() {
            if let Some(worker) = self.worker.as_mut() {
                if let Some(handle) = worker.handle.take() {
                    let _ = handle.join();
                }
            }

            self.worker = None;

            self.state = PlaybackState::Stopped;
        }

        result
    }

    /// Pause playback.
    pub fn pause(&mut self) {
        if self.state != PlaybackState::Playing {
            return;
        }

        if let Some(worker) = self.worker.as_ref() {
            if worker.send(WorkerCommand::Pause).is_ok() {
                self.state = PlaybackState::Paused;
            }
        }
    }

    /// Resume paused playback.
    pub fn resume(&mut self) {
        if self.state != PlaybackState::Paused {
            return;
        }

        if let Some(worker) = self.worker.as_ref() {
            if worker.send(WorkerCommand::Resume).is_ok() {
                self.state = PlaybackState::Playing;
            }
        }
    }

    /// Request a relative seek. The playback worker handles the demuxer seek
    /// asynchronously; positive offsets seek forward and negative offsets
    /// seek backward. The target is clamped to the track duration.
    pub fn seek_by_seconds(&mut self, offset_seconds: i64) -> Result<(), Box<dyn Error>> {
        if !matches!(self.state, PlaybackState::Playing | PlaybackState::Paused) {
            return Err("Cannot seek unless playback is active".into());
        }

        let current = self.position();
        let target = seek_target(current, offset_seconds, self.metadata.duration);
        let worker = self
            .worker
            .as_ref()
            .ok_or("Playback worker is not running")?;

        {
            let mut pending_seek = self
                .pending_seek
                .lock()
                .map_err(|_| "Pending seek lock poisoned")?;
            *pending_seek = Some(target);
        }

        if let Err(error) = worker.send(WorkerCommand::Seek) {
            if let Ok(mut pending_seek) = self.pending_seek.lock() {
                *pending_seek = None;
            }

            return Err(error);
        }

        self.played_frames.store(
            duration_to_frames(target, self.output_spec.sample_rate),
            Ordering::Relaxed,
        );

        Ok(())
    }

    /// Stop playback.
    pub fn stop(&mut self) {
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.send(WorkerCommand::Stop);
        }

        self.state = PlaybackState::Stopped;
    }

    /// Wait for playback to finish.
    ///
    /// This is retained for callers that want
    /// the old blocking behavior.
    pub fn wait(&mut self) -> Result<(), Box<dyn Error>> {
        match self.wait_end()? {
            PlaybackEndReason::Completed => println!("Playback complete."),
            PlaybackEndReason::Stopped => {}
        }

        Ok(())
    }

    /// Wait for playback to finish and return why the worker ended.
    pub fn wait_end(&mut self) -> Result<PlaybackEndReason, Box<dyn Error>> {
        let worker = match self.worker.as_mut() {
            Some(worker) => worker,

            None => return Ok(PlaybackEndReason::Stopped),
        };

        let result = worker.wait()?;

        self.worker = None;

        self.state = PlaybackState::Stopped;

        match result {
            WorkerResult::Completed => Ok(PlaybackEndReason::Completed),

            WorkerResult::Stopped => Ok(PlaybackEndReason::Stopped),

            WorkerResult::Failed(error) => Err(error.into()),
        }
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Run the complete audio pipeline on the
/// dedicated playback worker thread.
fn run_playback_pipeline(
    path: &str,
    command_rx: &mpsc::Receiver<WorkerCommand>,
    played_frames: Arc<AtomicU64>,
    pending_seek: Arc<Mutex<Option<Duration>>>,
    spectrum: Arc<SpectrumShared>,
    volume: Arc<AtomicU32>,
) -> Result<WorkerPipelineResult, Box<dyn Error>> {
    let mut decoder = AudioDecoder::open(path)?;

    let output = AudioOutput::open_with_spectrum(
        decoder.spec(),
        Arc::clone(&played_frames),
        spectrum,
        volume,
    )?;

    let output_spec = output.spec();

    let channel_conversion = decoder.spec().channels != output_spec.channels;

    let resampling = decoder.spec().sample_rate != output_spec.sample_rate;

    let channel_output_spec = AudioSpec::new(decoder.spec().sample_rate, output_spec.channels);

    let mut resampler = if resampling {
        Some(AudioResampler::new(channel_output_spec, output_spec)?)
    } else {
        None
    };

    'playback: loop {
        'decode: loop {
            match command_rx.try_recv() {
                Ok(WorkerCommand::Pause) => {
                    output.pause()?;
                    let mut seeked_while_paused = false;

                    loop {
                        match command_rx.recv()? {
                            WorkerCommand::Resume => {
                                output.resume()?;

                                if seeked_while_paused {
                                    continue 'playback;
                                }

                                break;
                            }

                            WorkerCommand::Seek => {
                                if let Some(target) = take_pending_seek(&pending_seek) {
                                    apply_seek(
                                        target,
                                        &mut decoder,
                                        &output,
                                        channel_conversion,
                                        channel_output_spec,
                                        resampler.as_mut(),
                                        &played_frames,
                                    )?;

                                    seeked_while_paused = true;
                                }
                            }

                            WorkerCommand::Stop | WorkerCommand::Shutdown => {
                                output.stop()?;

                                return Ok(WorkerPipelineResult::Stopped);
                            }

                            WorkerCommand::Pause => {}
                        }
                    }
                }

                Ok(WorkerCommand::Stop | WorkerCommand::Shutdown) => {
                    output.stop()?;

                    return Ok(WorkerPipelineResult::Stopped);
                }

                Ok(WorkerCommand::Resume) => {
                    output.resume()?;
                }

                Ok(WorkerCommand::Seek) => {
                    if let Some(target) = take_pending_seek(&pending_seek) {
                        apply_seek(
                            target,
                            &mut decoder,
                            &output,
                            channel_conversion,
                            channel_output_spec,
                            resampler.as_mut(),
                            &played_frames,
                        )?;
                    } else {
                        continue 'decode;
                    }
                }

                Err(mpsc::TryRecvError::Empty) => {}

                Err(mpsc::TryRecvError::Disconnected) => {
                    output.stop()?;

                    return Ok(WorkerPipelineResult::Stopped);
                }
            }

            let chunk = match decode_next_output_chunk(
                &mut decoder,
                channel_conversion,
                channel_output_spec,
                resampler.as_mut(),
            )? {
                Some(chunk) => chunk,

                None => break,
            };

            if !chunk.samples.is_empty() {
                output.push_samples(&chunk.samples);
            }
        }

        if let Some(resampler) = resampler.as_mut() {
            let tail = resampler.flush()?;

            if !tail.samples.is_empty() {
                output.push_samples(&tail.samples);
            }
        }

        while output.pending_samples() > 0 {
            match command_rx.try_recv() {
                Ok(WorkerCommand::Pause) => {
                    output.pause()?;
                    let mut seeked_while_paused = false;

                    loop {
                        match command_rx.recv()? {
                            WorkerCommand::Resume => {
                                output.resume()?;

                                if seeked_while_paused {
                                    continue 'playback;
                                }

                                break;
                            }

                            WorkerCommand::Seek => {
                                if let Some(target) = take_pending_seek(&pending_seek) {
                                    apply_seek(
                                        target,
                                        &mut decoder,
                                        &output,
                                        channel_conversion,
                                        channel_output_spec,
                                        resampler.as_mut(),
                                        &played_frames,
                                    )?;

                                    seeked_while_paused = true;
                                }
                            }

                            WorkerCommand::Stop | WorkerCommand::Shutdown => {
                                output.stop()?;

                                return Ok(WorkerPipelineResult::Stopped);
                            }

                            WorkerCommand::Pause => {}
                        }
                    }
                }

                Ok(WorkerCommand::Stop | WorkerCommand::Shutdown) => {
                    output.stop()?;

                    return Ok(WorkerPipelineResult::Stopped);
                }

                Ok(WorkerCommand::Resume) => {
                    output.resume()?;
                }

                Ok(WorkerCommand::Seek) => {
                    if let Some(target) = take_pending_seek(&pending_seek) {
                        apply_seek(
                            target,
                            &mut decoder,
                            &output,
                            channel_conversion,
                            channel_output_spec,
                            resampler.as_mut(),
                            &played_frames,
                        )?;

                        continue 'playback;
                    }
                }

                Err(mpsc::TryRecvError::Empty) => {}

                Err(mpsc::TryRecvError::Disconnected) => {
                    output.stop()?;

                    return Ok(WorkerPipelineResult::Stopped);
                }
            }

            if output.needs_rebuild() {
                return Err("Audio device changed. Stream needs rebuilding.".into());
            }

            thread::sleep(Duration::from_millis(10));
        }

        thread::sleep(Duration::from_millis(50));

        if output.needs_rebuild() {
            return Err("Audio device changed. Stream needs rebuilding.".into());
        }

        output.stop()?;

        return Ok(WorkerPipelineResult::Completed);
    }
}

fn apply_seek(
    target: Duration,
    decoder: &mut AudioDecoder,
    output: &AudioOutput,
    channel_conversion: bool,
    channel_output_spec: AudioSpec,
    resampler: Option<&mut AudioResampler>,
    played_frames: &AtomicU64,
) -> Result<(), Box<dyn Error>> {
    let resume_after_seek = output.is_playing();
    output.pause()?;

    output.clear_buffer()?;

    let actual = decoder.seek_to(target)?;

    let mut resampler = resampler;
    if let Some(resampler) = resampler.as_mut() {
        resampler.reset();
    }

    played_frames.store(
        duration_to_frames(actual, output.spec().sample_rate),
        Ordering::Relaxed,
    );

    if let Some(pcm) =
        decode_next_output_chunk(decoder, channel_conversion, channel_output_spec, resampler)?
    {
        if !pcm.samples.is_empty() {
            output.push_samples(&pcm.samples);
        }

        if resume_after_seek {
            output.resume()?;
        }
    }
    Ok(())
}

fn decode_next_output_chunk(
    decoder: &mut AudioDecoder,
    channel_conversion: bool,
    channel_output_spec: AudioSpec,
    resampler: Option<&mut AudioResampler>,
) -> Result<Option<super::types::PcmBuffer>, Box<dyn Error>> {
    let Some(chunk) = decoder.next_chunk()? else {
        return Ok(None);
    };

    let pcm = if channel_conversion {
        AudioChannelConverter::convert(&chunk, channel_output_spec)?
    } else {
        chunk
    };

    let pcm = if let Some(resampler) = resampler {
        let frames = pcm.frame_count();
        let expected = resampler.input_frames_next();

        if frames == expected {
            resampler.process(&pcm)?
        } else {
            resampler.process_final(&pcm)?
        }
    } else {
        pcm
    };

    Ok(Some(pcm))
}

fn take_pending_seek(pending_seek: &Mutex<Option<Duration>>) -> Option<Duration> {
    pending_seek
        .lock()
        .ok()
        .and_then(|mut target| target.take())
}

fn seek_target(current: Duration, offset_seconds: i64, total: Option<Duration>) -> Duration {
    let offset = Duration::from_secs(offset_seconds.unsigned_abs());
    let target = if offset_seconds < 0 {
        current.saturating_sub(offset)
    } else {
        current.saturating_add(offset)
    };

    total.map_or(target, |duration| target.min(duration))
}

fn duration_to_frames(position: Duration, sample_rate: u32) -> u64 {
    (position.as_nanos() * u128::from(sample_rate) / 1_000_000_000).min(u128::from(u64::MAX)) as u64
}

fn frames_to_duration(frames: u64, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }

    let rate = u64::from(sample_rate);
    let seconds = frames / rate;
    let nanoseconds = ((frames % rate) * 1_000_000_000 / rate) as u32;
    Duration::new(seconds, nanoseconds)
}

#[cfg(test)]
mod seek_tests {
    use super::{seek_target, take_pending_seek};
    use std::{sync::Mutex, time::Duration};

    #[test]
    fn seek_backward_clamps_to_zero() {
        assert_eq!(
            seek_target(Duration::from_secs(3), -5, Some(Duration::from_secs(200))),
            Duration::ZERO
        );
    }

    #[test]
    fn seek_forward_clamps_to_duration() {
        assert_eq!(
            seek_target(Duration::from_secs(198), 5, Some(Duration::from_secs(200))),
            Duration::from_secs(200)
        );
    }

    #[test]
    fn seek_forward_advances_by_requested_offset() {
        assert_eq!(
            seek_target(Duration::from_secs(42), 5, Some(Duration::from_secs(200))),
            Duration::from_secs(47)
        );
    }

    #[test]
    fn pending_seek_uses_latest_requested_target() {
        let pending = Mutex::new(Some(Duration::from_secs(42)));
        *pending.lock().unwrap() = Some(Duration::from_secs(47));

        assert_eq!(take_pending_seek(&pending), Some(Duration::from_secs(47)));
        assert_eq!(take_pending_seek(&pending), None);
    }
}
