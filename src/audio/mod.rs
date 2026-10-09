//! Realtime audio: a single [`SiONDriver`] streaming music on the main
//! thread, UI SFX fired onto disposable tracks while it plays. Native
//! build pumps a cpal output through an interleaved-f32 ring buffer;
//! the web export renders into a static staging block that JS audio
//! callbacks copy out (see `src/bin/title_menu.rs`).

pub mod sfx;

use std::cell::{Cell, RefCell};
#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;
use std::rc::Rc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex};

use libsion::core::core as sion;
use libsion::{DriverEvent, SiONData, SiONDriver};

pub use sfx::{SfxBank, SfxId};

pub const SAMPLE_RATE: i32 = 44100;
pub const CHANNELS: usize = 2;
/// Driver block length in frames (stereo samples are x2).
pub const BLOCK_FRAMES: usize = 2048;

/// Written by the driver's `on_event` closure, read by the frame loop.
/// The closure must never touch the driver itself (it fires while the
/// driver is mid-`update`).
#[derive(Default)]
struct StreamState {
    song_finished: Cell<bool>,
}

pub struct AudioEngine {
    driver: SiONDriver,
    scratch: Vec<f32>,
    music: Option<Rc<RefCell<SiONData>>>,
    state: Rc<StreamState>,
    loop_music: Cell<bool>,
    sfx_bank: SfxBank,
    /// Independent music level applied to MML track master volumes — the
    /// driver's own output volume stays at unity so SFX never ride along.
    music_gain: Cell<f32>,
    /// Authored (pre-gain) master volume per music-track slot; empty until
    /// the first processed block after (re)`play` captures it.
    music_base: RefCell<Vec<i32>>,
    #[cfg(not(target_arch = "wasm32"))]
    ring: Arc<Mutex<VecDeque<f32>>>,
}

impl AudioEngine {
    /// Builds the driver, compiles music and the SFX bank exactly once; the
    /// live path only ever triggers precompiled data afterwards. `sfx_sources`
    /// are `(id, SiMML text)` pairs loaded from assets by the host (see
    /// [`crate::config`]); entries may be omitted to skip an effect. `mml` is
    /// `None` when the music asset is missing: no track is compiled and every
    /// start/loop call stays inert (no built-in fallback tune). Must run
    /// before `start_music` because `compile()` stops the stream.
    pub fn new(mml: Option<String>, sfx_sources: &[(SfxId, String)]) -> Self {
        sion::initialize();

        let mut driver = SiONDriver::new(BLOCK_FRAMES as i32, CHANNELS as i32, SAMPLE_RATE, 0);
        driver.set_auto_stop(true);
        driver.set_max_track_count(96);

        let state = Rc::new(StreamState::default());
        {
            let shared = state.clone();
            driver.on_event = Some(Box::new(move |event: &DriverEvent| {
                match event.get_event_type().as_str() {
                    "stream_started" => shared.song_finished.set(false),
                    "stream_stopped" | "finished_playback" => shared.song_finished.set(true),
                    _ => {}
                }
            }));
        }

        let music = mml.map(|m| driver.compile(m));
        let sfx_bank = SfxBank::precompile(&mut driver, sfx_sources);

        AudioEngine {
            driver,
            scratch: vec![0.0f32; BLOCK_FRAMES * CHANNELS],
            music,
            state,
            loop_music: Cell::new(true),
            sfx_bank,
            music_gain: Cell::new(1.0),
            music_base: RefCell::new(Vec::new()),
            #[cfg(not(target_arch = "wasm32"))]
            ring: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Start (or restart) the title theme, looping forever.
    pub fn start_music(&mut self) {
        if let Some(data) = &self.music {
            let data = data.clone();
            self.state.song_finished.set(false);
            self.loop_music.set(true);
            self.driver.play(&data, true);
        }
    }

    /// Silence future music loops; keeps any scheduled tail audible.
    pub fn stop_music_looping(&mut self) {
        self.loop_music.set(false);
    }

    /// Music-only level (0..1) applied to the song tracks' master volumes.
    /// Disposable SFX voices are separate tracks, so quiet music + loud SFX
    /// is fully expressible; the driver's global output stays at unity.
    pub fn set_music_volume(&mut self, p_value: f32) {
        self.music_gain.set(p_value.clamp(0.0, 1.0));
        self.apply_music_volume();
    }

    pub fn music_volume(&self) -> f32 {
        self.music_gain.get()
    }

    /// Write `baseline × gain` onto every known music track. A no-op until
    /// the first baseline capture (song tracks exist one block after `play`).
    fn apply_music_volume(&mut self) {
        let gain = self.music_gain.get();
        if self.music_base.borrow().is_empty() {
            return;
        }
        let tracks = self.driver.music_tracks();
        let base = self.music_base.borrow();
        for (slot, track) in tracks.iter().enumerate() {
            if let Some(&authored) = base.get(slot) {
                let value = (authored as f32 * gain).round() as i32;
                track.borrow_mut().set_master_volume(value.clamp(0, 128));
            }
        }
    }

    /// Snapshot the freshly `play`ed tracks' authored volumes (MML headers,
    /// including any per-track `v`, have applied by the time this runs —
    /// one processed block after `play`) and bring them to slider level.
    fn capture_music_baseline(&mut self) {
        let tracks = self.driver.music_tracks();
        if tracks.is_empty() {
            return;
        }
        let authored: Vec<i32> = tracks
            .iter()
            .map(|t| t.borrow().get_master_volume())
            .collect();
        *self.music_base.borrow_mut() = authored;
        drop(tracks);
        self.apply_music_volume();
    }

    /// One-shot UI effect from the precompiled bank (`sequence_on` only).
    pub fn play_sfx(&mut self, id: SfxId, sfx_gain: f32) {
        self.sfx_bank.play(&mut self.driver, id, sfx_gain);
    }

    /// Programmatic pitch-following click for sliders (0.0..=1.0).
    pub fn slider_blip(&mut self, norm: f32, sfx_gain: f32) {
        self.sfx_bank.slider_blip(&mut self.driver, norm, sfx_gain);
    }

    pub fn fade_out_master(&mut self, seconds: f64) {
        if self.driver.is_streaming() {
            self.driver.fade_out(seconds);
        }
    }

    /// Advance the sequencer once per rendered block; re-`play` the
    /// (precompiled) theme when its loop window rolls over.
    fn pump_sequencer(&mut self) {
        if self.loop_music.get()
            && self.state.song_finished.get()
            && !self.driver.is_streaming()
        {
            eprintln!("title theme finished - looping");
            if let Some(data) = &self.music {
                let data = data.clone();
                // fresh tracks next block -> re-capture their authored gain
                self.music_base.borrow_mut().clear();
                self.driver.play(&data, false);
            }
        }

        if self.music.is_some() {
            self.driver.update();
            // first processed block after (re)`play`: song tracks now exist
            // with MML headers applied, so snapshot then apply the slider.
            if self.music_base.borrow().is_empty() {
                self.capture_music_baseline();
            } else {
                // keep tracks pinned to baseline x gain across the block
                self.apply_music_volume();
            }
        }
    }

    // --- native output ----------------------------------------------------

    /// Call once per main-loop frame: keep at least `keep_frames` frames of
    /// audio queued for the cpal callback.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn tick_native(&mut self, keep_frames: usize) {
        let want = (keep_frames * CHANNELS).min(BLOCK_FRAMES * CHANNELS);

        let mut queued = self.ring.lock().expect("audio ring lock poisoned").len();
        while queued < want {
            self.pump_sequencer();
            let take = (want - queued).min(BLOCK_FRAMES * CHANNELS);
            self.driver.render_chunk(&mut self.scratch[..take]);

            let mut ring = self.ring.lock().expect("audio ring lock poisoned");
            if ring.len() > want * 4 {
                let excess = (ring.len() - want * 4).min(ring.len());
                for _ in 0..excess {
                    ring.pop_front();
                }
            }
            let added = take;
            ring.extend(self.scratch[..take].iter().copied());
            drop(ring);
            queued += added;
        }

        if self.driver.is_streaming() {
            // keep the pump alive even when the ring is momentarily full
        } else if !self.loop_music.get() {
            // deliberate stop (exit fade): nothing more to queue.
        }
    }

    /// Starts the cpal stream wired to this engine's ring. The returned
    /// stream must be kept alive by the caller (`_stream`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start_native_output(&mut self) -> Result<native_sink::OutputStream, String> {
        native_sink::open_stream(&self.ring)
    }

    /// Sleep briefly while waiting for the queued tail to drain on exit.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn drain_native_output(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline
            && !self.ring.lock().expect("audio ring lock poisoned").is_empty()
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    // --- web output --------------------------------------------------------

    /// Render exactly `frames` frames (stereo interleaved) into the staging
    /// block and return its raw pointer for JS to copy. `render_chunk`
    /// zero-fills itself while idle, so silence needs no special case.
    #[cfg(target_arch = "wasm32")]
    pub fn pump_web(&mut self, frames: usize) -> *const f32 {
        let mut done = 0usize;
        while done < frames {
            self.pump_sequencer();
            let take = (frames - done).min(BLOCK_FRAMES);
            let start = done * CHANNELS;
            self.driver.render_chunk(&mut self.scratch[start..start + take * CHANNELS]);
            done += take;
        }
        self.scratch.as_ptr()
    }

    pub fn is_streaming(&self) -> bool {
        self.driver.is_streaming()
    }

    /// Advance the sequencer + render `blocks` fixed-size blocks without any
    /// OS audio device (drives the same baseline/gain path as `tick_native`
    /// for deterministic tests). Discards samples.
    #[doc(hidden)]
    pub fn pump_for_test(&mut self, blocks: usize) {
        for _ in 0..blocks {
            self.pump_sequencer();
            let scratch = &mut self.scratch;
            self.driver.render_chunk(&mut scratch[..BLOCK_FRAMES * CHANNELS]);
        }
    }

    /// Live music-track master volumes (post-gain); empty until a baseline
    /// exists. Exposed so tests can assert music-only scaling.
    #[doc(hidden)]
    pub fn music_track_master_volumes(&self) -> Vec<i32> {
        self.driver.music_tracks().iter().map(|t| t.borrow().get_master_volume()).collect()
    }

    /// All live tracks (music + transient SFX), for test assertions.
    #[doc(hidden)]
    pub fn total_track_count(&self) -> i32 {
        self.driver.get_track_count()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        // Release every parser-arena consumer (compiled songs, bank data,
        // the event closure) before `sion::finalize()` kills statics; the
        // binary must drain native output BEFORE dropping.
        self.stop_music_looping();
        self.music = None;
        self.sfx_bank.clear_compiled();
        self.driver.on_event = None;
        self.driver.stop();
    }
}

// ---------------------------------------------------------------------------
// thread-local engine handle (browser JS pump + macroquad loop share it)
// ---------------------------------------------------------------------------

thread_local! {
    static ENGINE: RefCell<Option<AudioEngine>> = const { RefCell::new(None) };
}

pub fn install(engine: AudioEngine) {
    ENGINE.with(|slot| *slot.borrow_mut() = Some(engine));
}

pub fn with<R>(f: impl FnOnce(&mut AudioEngine) -> R) -> Option<R> {
    ENGINE.with(|slot| slot.borrow_mut().as_mut().map(f))
}

pub fn take() -> Option<AudioEngine> {
    ENGINE.with(|slot| slot.borrow_mut().take())
}

// ---------------------------------------------------------------------------
// native cpal sink (kept out of the web build)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
mod native_sink {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// Unified stream handle in cpal 0.18 (implements `StreamTrait`).
    pub type OutputStream = cpal::Stream;

    pub fn open_stream(ring: &Arc<Mutex<VecDeque<f32>>>) -> Result<OutputStream, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default audio output device")?;

        let config = device
            .supported_output_configs()
            .map_err(|e| format!("cannot query output configs: {e}"))?
            .find(|c| {
                c.channels() as usize == super::CHANNELS
                    && c.min_sample_rate() <= (super::SAMPLE_RATE as u32)
                    && c.max_sample_rate() >= (super::SAMPLE_RATE as u32)
                    && c.sample_format() == cpal::SampleFormat::F32
            })
            .ok_or("no f32 stereo 44100-capable output config")?
            .with_sample_rate(super::SAMPLE_RATE as u32)
            .config();

        let cb_ring = ring.clone();
        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut queue = cb_ring.lock().expect("audio ring lock poisoned");
                    for sample in data.iter_mut() {
                        *sample = queue.pop_front().unwrap_or(0.0);
                    }
                },
                |err| eprintln!("[cpal] {err}"),
                None, // cpal 0.18: no stream timeout
            )
            .map_err(|e| format!("cannot build output stream: {e}"))?;

        stream
            .play()
            .map_err(|e| format!("cannot start output stream: {e}"))?;
        Ok(stream)
    }
}
