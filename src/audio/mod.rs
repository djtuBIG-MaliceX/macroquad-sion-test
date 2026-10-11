//! Realtime audio: a single [`SiONDriver`] streaming music on the main
//! thread, UI SFX fired onto disposable tracks while it plays. Native
//! build pumps a cpal output through an interleaved-f32 ring buffer;
//! the web export renders into a static staging block that JS audio
//! callbacks copy out (see `src/bin/title_menu.rs`).
//!
//! Sampled assets (`assets/**.wav/.ogg`, plus browser-decoded `.mp3`) live
//! beside the driver in [`samples`] — a thin façade over `macroquad::audio`
//! whose decoders are quad-snd/audrey natively and WebAudio in the browser
//! (via the plugin shim in `web/index.html`). The [`play_track`] /
//! [`sfx_file`] façade routes by file extension so screens and scenario
//! scripts address both backends with one vocabulary, and volume/fade
//! helpers keep the two buses moving together.
//!
//! MIDI files (`assets/**.mid`) form a third route: [`midi`] parses the
//! SMF and streams it through driver tracks reserved for the app (bends,
//! expression and per-channel GM voices included), pumped in step with the
//! driver blocks.
//!
//! Music continuity: whatever owns the music bus, replaying the track that
//! is already sounding is a no-op — screen switches re-request their theme
//! every time, and only an actual track change restarts playback.

pub mod midi;
pub mod samples;
pub mod sfx;

use std::cell::{Cell, RefCell};
#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;
use std::collections::HashMap;
use std::rc::Rc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex};

use libsion::core::core as sion;
use libsion::{DriverEvent, SiONData, SiONDriver};

pub use midi::{MidiPlayer, MidiSong};
pub use sfx::{SfxBank, SfxId};

pub const SAMPLE_RATE: i32 = 44100;
pub const CHANNELS: usize = 2;
/// Driver block length in frames (stereo samples are x2).
pub const BLOCK_FRAMES: usize = 2048;

/// A loaded audio asset, classified by flavour at the loader boundary.
#[derive(Debug)]
pub enum AudioAsset {
    /// SiMML source text (`*.mml`): compiled onto the driver.
    Mml(String),
    /// Standard MIDI File bytes (`*.mid/.midi`): parsed, played on
    /// reserved tracks (see [`midi`]).
    Midi(Vec<u8>),
    /// Compressed/PCM container bytes: decoded by `macroquad::audio`.
    Bytes(Vec<u8>),
}

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
    state: Rc<StreamState>,
    loop_music: Cell<bool>,
    sfx_bank: SfxBank,
    /// Independent music level applied to MML track master volumes — the
    /// driver's own output volume stays at unity so SFX never ride along.
    music_gain: Cell<f32>,
    /// Authored (pre-gain) master volume per music-track slot; empty until
    /// the first processed block after (re)`play` captures it.
    music_base: RefCell<Vec<i32>>,

    /// Compiled SiMML songs keyed by asset path; `song_order` keeps the
    /// registration order so `start_music` can pick the first one.
    songs: HashMap<String, Rc<RefCell<SiONData>>>,
    song_order: Vec<String>,
    /// Scenario-referenced MML one-shots (path → data + max length).
    file_effects: HashMap<String, (Rc<RefCell<SiONData>>, f64)>,
    /// Parsed MIDI songs keyed by asset path (played on reserved tracks).
    midi_songs: HashMap<String, Rc<MidiSong>>,
    /// Live MIDI playback, if the current music is a `.mid` file.
    midi: Option<MidiPlayer>,
    current_music: RefCell<Option<String>>,

    /// Output level applied after the driver, so an exit fade sweeps every
    /// SiON bus at once (sampled audio fades via [`samples`]).
    master: Cell<f32>,
    master_step: Cell<f32>,

    #[cfg(not(target_arch = "wasm32"))]
    ring: Arc<Mutex<VecDeque<f32>>>,
}

impl AudioEngine {
    /// Builds the driver and precompiles the bank SFX exactly once; the
    /// live path only ever triggers precompiled data afterwards. `sfx_sources`
    /// are `(id, SiMML text)` pairs loaded from assets by the host (see
    /// [`crate::config`]); entries may be omitted to skip an effect. Songs and
    /// scenario MML one-shots arrive through `add_*` registrations — all of
    /// them must precede [`AudioEngine::play_music`] because `compile()`
    /// stops the live stream.
    pub fn new(sfx_sources: &[(SfxId, String)]) -> Self {
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

        let sfx_bank = SfxBank::precompile(&mut driver, sfx_sources);

        AudioEngine {
            driver,
            scratch: vec![0.0f32; BLOCK_FRAMES * CHANNELS],
            state,
            loop_music: Cell::new(true),
            sfx_bank,
            music_gain: Cell::new(1.0),
            music_base: RefCell::new(Vec::new()),
            songs: HashMap::new(),
            song_order: Vec::new(),
            file_effects: HashMap::new(),
            midi_songs: HashMap::new(),
            midi: None,
            current_music: RefCell::new(None),
            master: Cell::new(1.0),
            master_step: Cell::new(0.0),
            #[cfg(not(target_arch = "wasm32"))]
            ring: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Register a looping SiMML song under its asset path. Compiling stops
    /// the live stream, so mid-session registrations let the current song's
    /// loop resync it on the next pumped block.
    pub fn add_song(&mut self, path: &str, mml: String) {
        let data = self.driver.compile(mml);
        if self.songs.insert(path.to_string(), data).is_none() {
            self.song_order.push(path.to_string());
        }
    }

    /// Register a parsed MIDI song under its asset path. Parse-only: no
    /// `compile()` (which would stop the live stream), so `.mid` files can
    /// be registered mid-session safely too.
    pub fn add_midi_song(&mut self, path: &str, bytes: &[u8]) {
        match MidiSong::parse(bytes) {
            Ok(song) => {
                self.midi_songs.insert(path.to_string(), Rc::new(song));
            }
            Err(e) => eprintln!("midi asset '{path}' unparseable: {e}"),
        }
    }

    /// Register scenario-referenced MML one-shot; `beats` bounds the
    /// `sequence_on` window (16th-note beats, cf. [`SfxId::max_length`]).
    pub fn add_effect_mml(&mut self, path: &str, mml: String, beats: f64) {
        let data = self.driver.compile(mml);
        self.file_effects.insert(path.to_string(), (data, beats.max(1.0)));
    }

    /// Start (or restart from the top) the registered SiMML/MIDI track for
    /// `path` — unless that very track is already sounding: screen switches
    /// replay their screen theme, and a restart there would be the old
    /// stutter; only an actual track change restarts from zero. Unknown/
    /// missing tracks mean silence: nothing is substituted. Sampled files
    /// belong to [`samples`] — go through [`play_track`].
    pub fn play_music(&mut self, path: &str) {
        if self.current_music.borrow().as_deref() == Some(path) && self.driver.is_streaming() {
            return; // already sounding: keep playing (no restart)
        }

        // Whatever played before leaves the stage now; a live MIDI player
        // drops with its reserved tracks inside the coming `stream()`.
        self.midi = None;
        self.driver.set_auto_stop(true);

        if let Some(data) = self.songs.get(path).cloned() {
            *self.current_music.borrow_mut() = Some(path.to_string());
            self.state.song_finished.set(false);
            self.loop_music.set(true);
            // fresh tracks next block -> re-capture their authored gain
            self.music_base.borrow_mut().clear();
            self.driver.play(&data, true);
        } else if let Some(song) = self.midi_songs.get(path).cloned() {
            *self.current_music.borrow_mut() = Some(path.to_string());
            self.state.song_finished.set(false);
            self.loop_music.set(true);
            self.music_base.borrow_mut().clear();
            // Silent gaps between MIDI notes would trip the auto-stop (all
            // reserved tracks report finished), so streaming is held open.
            self.driver.set_auto_stop(false);
            // `stream` frees every track (MML included), then the player
            // reserves fresh USER_CONTROLLED tracks on the empty stage.
            self.driver.stream(true);
            self.midi = Some(MidiPlayer::start(&mut self.driver, song));
        } else {
            eprintln!("music asset '{path}' unavailable - silence");
            *self.current_music.borrow_mut() = None;
            self.loop_music.set(false);
            self.driver.stop();
        }
    }

    /// Play the first registered song (title screen convention).
    pub fn start_music(&mut self) {
        if let Some(first) = self.song_order.first().cloned() {
            self.play_music(&first);
        }
    }

    /// The path currently sounding on the driver, if any.
    pub fn current_music(&self) -> Option<String> {
        self.current_music.borrow().clone()
    }

    /// True when the given path is registered as a compiled or MIDI song.
    pub fn has_track(&self, path: &str) -> bool {
        self.songs.contains_key(path)
            || self.file_effects.contains_key(path)
            || self.midi_songs.contains_key(path)
    }

    /// Silence future music loops; keeps any scheduled tail audible.
    pub fn stop_music_looping(&mut self) {
        self.loop_music.set(false);
    }

    /// Music-only level (0..1) applied to the song tracks' master volumes.
    /// Disposable SFX voices are separate, so quiet music + loud SFX is
    /// fully expressible; the driver's global output stays at unity.
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

    /// One-shot MML effect referenced by asset path: bank ids route to
    /// their precompiled entry, scenario MML files use their registration
    /// window. Sampled paths never reach this — [`sfx_file`] splits first,
    /// and unknown paths stay silent (a warning is printed).
    pub fn play_file_sfx(&mut self, path: &str, sfx_gain: f32) {
        if let Some(id) = self.sfx_bank.id_for_path(path) {
            self.play_sfx(id, sfx_gain);
            return;
        }
        if let Some((data, beats)) = self.file_effects.get(path).cloned() {
            if self.driver.is_streaming() {
                let tracks = self.driver.sequence_on(&data, None, beats, 0.0, 0.0, -1, true);
                for track in &tracks {
                    let mut tb = track.borrow_mut();
                    let base = tb.get_master_volume().max(1) as f32;
                    tb.set_master_volume((base * sfx_gain) as i32);
                }
            }
            return;
        }
        eprintln!("sfx asset '{path}' unavailable - skipped");
    }

    /// Programmatic pitch-following click for sliders (0.0..=1.0).
    pub fn slider_blip(&mut self, norm: f32, sfx_gain: f32) {
        self.sfx_bank.slider_blip(&mut self.driver, norm, sfx_gain);
    }

    /// Driver-side fade to silence over `seconds`, then hold. The matching
    /// sampled-bus sweep lives in [`samples::fade_out_all`].
    pub fn fade_out_master(&mut self, seconds: f64) {
        if self.master.get() <= 0.0 || self.master_step.get() < 0.0 {
            return; // already faded / fading
        }
        let secs = seconds.max(0.05) as f32;
        self.master_step.set(-1.0 / (secs * SAMPLE_RATE as f32));
    }

    /// Advance the sequencer once per rendered block; re-`play` the
    /// (precompiled) current song when its loop window rolls over. Also the
    /// one unconditional driver step, so file-SFX-only sessions (no music
    /// registered at all) still advance the chip.
    fn pump_sequencer(&mut self) {
        // MIDI playback drives reserved tracks directly: pump it per block
        // (note scheduling + realtime controls + CC7×gain volumes) and skip
        // the MML loop/baseline machinery entirely — `music_tracks()` only
        // enumerates compiled-song tracks, which a MIDI stream has none of.
        if let Some(player) = self.midi.as_mut() {
            player.tick(&mut self.driver, self.music_gain.get());
            self.driver.update();
            return;
        }

        let current_is_song = self.current_music.borrow().as_deref().is_some_and(|p| self.songs.contains_key(p));
        if current_is_song
            && self.loop_music.get()
            && self.state.song_finished.get()
            && !self.driver.is_streaming()
        {
            let path = self.current_music.borrow().clone().unwrap_or_default();
            eprintln!("music '{path}' finished - looping");
            let data = self.songs.get(&path).cloned().expect("song just checked");
            // fresh tracks next block -> re-capture their authored gain
            self.music_base.borrow_mut().clear();
            self.driver.play(&data, false);
        }

        self.driver.update();
        if current_is_song {
            // first processed block after (re)`play`: song tracks now exist
            // with MML headers applied, so snapshot then apply the slider.
            if self.music_base.borrow().is_empty() {
                self.capture_music_baseline();
            } else {
                // keep tracks pinned to baseline x gain across the block
                self.apply_music_volume();
            }
        } else {
            self.music_base.borrow_mut().clear();
        }
    }

    /// Apply the engine master (exit) fade to one block of driver output.
    fn apply_master(&mut self, buf: &mut [f32]) {
        let master = self.master.get();
        let step = self.master_step.get();
        if master < 1.0 || step != 0.0 {
            for s in buf.iter_mut() {
                *s *= master;
            }
            if step < 0.0 {
                let next = (master + step * (buf.len() as f32)).max(0.0);
                self.master.set(next);
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

            // Scratch leaves the engine for one block so `apply_master` can
            // hold &mut self while writing into it (no aliasing borrows).
            let mut buf = std::mem::take(&mut self.scratch);
            self.driver.render_chunk(&mut buf[..take]);
            self.apply_master(&mut buf[..take]);

            let mut ring = self.ring.lock().expect("audio ring lock poisoned");
            if ring.len() > want * 4 {
                let excess = (ring.len() - want * 4).min(ring.len());
                for _ in 0..excess {
                    ring.pop_front();
                }
            }
            let added = take;
            ring.extend(buf[..take].iter().copied());
            drop(ring);
            std::mem::swap(&mut self.scratch, &mut buf);
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
        let mut buf = std::mem::take(&mut self.scratch);
        let mut done = 0usize;
        while done < frames {
            self.pump_sequencer();
            let take = (frames - done).min(BLOCK_FRAMES);
            let start = done * CHANNELS;
            self.driver.render_chunk(&mut buf[start..start + take * CHANNELS]);
            self.apply_master(&mut buf[start..start + take * CHANNELS]);
            done += take;
        }
        let ptr = buf.as_ptr();
        std::mem::swap(&mut self.scratch, &mut buf);
        ptr
    }

    pub fn is_streaming(&self) -> bool {
        self.driver.is_streaming()
    }

    /// Advance the sequencer + render `blocks` fixed-size blocks without any
    /// OS audio device (drives the same baseline/gain path as `tick_native`
    /// for deterministic tests). Discards samples.
    #[doc(hidden)]
    pub fn pump_for_test(&mut self, blocks: usize) {
        let mut buf = std::mem::take(&mut self.scratch);
        for _ in 0..blocks {
            self.pump_sequencer();
            self.driver.render_chunk(&mut buf[..BLOCK_FRAMES * CHANNELS]);
            self.apply_master(&mut buf[..BLOCK_FRAMES * CHANNELS]);
        }
        std::mem::swap(&mut self.scratch, &mut buf);
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

    /// Reserved MIDI playback tracks currently live (0 when nothing plays).
    /// MIDI tracks are `USER_CONTROLLED`, never `MML_TRACK`, so they do not
    /// show up in [`Self::music_track_master_volumes`].
    #[doc(hidden)]
    pub fn midi_track_count(&self) -> usize {
        self.midi.as_ref().map_or(0, |p| p.track_count())
    }

    /// Engine audio clock in processed samples. Test observability for the
    /// no-restart music guard: `play`/`stream` reset it to zero, a same-path
    /// replay (the guard) lets it keep rising.
    #[doc(hidden)]
    pub fn processed_sample_count(&self) -> i64 {
        self.driver.sequencer.borrow().get_processed_sample_count() as i64
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        // Release every arena-backed consumer (compiled songs, bank data,
        // MIDI tracks/songs, the event closure) before `sion::finalize()`
        // kills statics; the binary must drain native output BEFORE dropping.
        self.stop_music_looping();
        self.midi = None;
        self.songs.clear();
        self.file_effects.clear();
        self.midi_songs.clear();
        *self.current_music.borrow_mut() = None;
        self.sfx_bank.clear_compiled();
        self.driver.on_event = None;
        self.driver.stop();
    }
}

// ---------------------------------------------------------------------------
// dual-bus façades: SiMML -> the driver, sampled files -> macroquad audio
// ---------------------------------------------------------------------------

/// Start (restart from zero) whichever backend owns `path`: compiled SiMML
/// on the driver, or a decoded sample through [`samples`]. Whatever the
/// other bus was looping is stopped first, and both honor the shared music
/// level so the Options slider stays authoritative everywhere.
pub fn play_track(path: &str) {
    if samples::is_sample_path(path) {
        let g = with(|e| e.music_volume()).unwrap_or(1.0);
        samples::set_music_gain(g);
        if !samples::play_music(path) {
            // unknown sample: silence here too, matching the MML policy
            with(|e| {
                e.stop_music_looping();
                e.driver_hush();
            });
        }
    } else {
        samples::stop_music();
        with(|e| e.play_music(path));
    }
}

/// One-shot scenario SFX routed by extension (bank/MML on the driver,
/// sampled through macroquad). `gain` is the live SFX slider value.
pub fn sfx_file(path: &str, gain: f32) {
    if samples::is_sample_path(path) {
        samples::play_sfx(path, gain);
    } else {
        with(|e| e.play_file_sfx(path, gain));
    }
}

/// Music bus level for both backends.
pub fn set_music_level(value: f32) {
    with(|e| e.set_music_volume(value));
    samples::set_music_gain(value);
}

/// Exit-fade helper: sweep driver + sampled buses to silence together.
pub fn fade_all_out(seconds: f64) {
    with(|e| {
        e.stop_music_looping();
        e.fade_out_master(seconds);
    });
    samples::fade_out_all(seconds);
}

/// Per-frame housekeeping for buses that run outside the driver pump.
pub fn tick(dt: f32) {
    samples::tick(dt);
}

// ---------------------------------------------------------------------------
// thread-local engine handle (browser JS pump + macroquad loop share it)
// ---------------------------------------------------------------------------

thread_local! {
    static ENGINE: RefCell<Option<AudioEngine>> = const { RefCell::new(None) };
}

impl AudioEngine {
    /// Immediately kill any streaming song (looping already disabled); used
    /// when a sampled track takes over the music bus. A live MIDI player
    /// drops with it — its reserved tracks die on the next `stream()`.
    fn driver_hush(&mut self) {
        self.midi = None;
        self.driver.set_auto_stop(true);
        *self.current_music.borrow_mut() = None;
        self.music_base.borrow_mut().clear();
        self.driver.stop();
    }
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
