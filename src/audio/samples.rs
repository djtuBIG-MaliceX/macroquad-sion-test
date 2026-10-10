//! Sampled-audio bus in front of `macroquad::audio`: `.wav`/`.ogg`
//! everywhere (audrey decoders natively, `decodeAudioData` in the browser),
//! `.mp3` and friends wherever the host JS can decode them. Scenario
//! `music:`/`sfx:` commands with sample extensions land here after the host
//! binary preloaded and registered them (browser decode is async, so it
//! happens in the same startup pass as everything else).
//!
//! Gain model: one-shot instances carry their SFX-slider volume baked into
//! `PlaySoundParams.volume` at trigger; the per-Sound multiplier
//! (`set_sound_volume`) carries engine master × music-bus level for the
//! looping track and pure master for effects, so a screen switch, slider
//! move or exit fade sweeps each bus correctly through the facade helpers.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use macroquad::audio::{self as mq, PlaySoundParams};

const SAMPLE_EXTS: &[&str] = &["wav", "ogg", "mp3", "flac", "aac", "m4a"];

thread_local! {
    static BANK: RefCell<HashMap<String, mq::Sound>> = RefCell::new(HashMap::new());
    static MUSIC: RefCell<Option<String>> = const { RefCell::new(None) };
    static MUSIC_GAIN: Cell<f32> = const { Cell::new(1.0) };
    static MASTER: Cell<f32> = const { Cell::new(1.0) };
    /// Linear fade rate (level lost per second); 0 while not fading out.
    static FADE_RATE: Cell<f32> = const { Cell::new(0.0) };
}

/// True for extensions the sampled bus owns (everything else is SiMML).
pub fn is_sample_path(path: &str) -> bool {
    path.rsplit('.').next().is_some_and(|ext| SAMPLE_EXTS.iter().any(|s| s.eq_ignore_ascii_case(ext)))
}

/// True when `path` was decoded and banked by [`register`].
pub fn has(path: &str) -> bool {
    BANK.with(|b| b.borrow().contains_key(path))
}

/// Decode `bytes` and bank them under `path`. Async because browsers decode
/// via WebAudio; failures stay silent assets with a printed warning (the
/// missing-asset policy of the whole project).
pub async fn register(path: &str, bytes: &[u8]) {
    match mq::load_sound_from_bytes(bytes).await {
        Ok(sound) => {
            BANK.with(|b| b.borrow_mut().insert(path.to_string(), sound));
        }
        Err(e) => eprintln!("sample asset '{path}' undecodable: {e:?}"),
    }
}

fn lookup(path: &str) -> Option<mq::Sound> {
    BANK.with(|b| b.borrow().get(path).cloned())
}

/// Loop `path` from the top; returns false (silence) when it was never
/// registered. The previous looping track — sampled or not, from the
/// facade's perspective — is stopped first so switches are clean.
pub fn play_music(path: &str) -> bool {
    stop_music();

    let Some(sound) = lookup(path) else {
        eprintln!("sample music '{path}' not registered - silence");
        return false;
    };
    let volume = MUSIC_GAIN.get() * MASTER.get();
    mq::play_sound(&sound, PlaySoundParams { looped: true, volume: 1.0 });
    mq::set_sound_volume(&sound, volume);
    MUSIC.with(|m| *m.borrow_mut() = Some(path.to_string()));
    true
}

/// Stop the looping track (scheduled one-shots keep playing out).
pub fn stop_music() {
    let prev = MUSIC.with(|m| m.borrow_mut().take());
    if let Some(path) = prev {
        if let Some(sound) = lookup(&path) {
            mq::stop_sound(&sound);
        }
    }
}

/// Music-bus level (0..1) from the Options slider, honored by any live loop.
pub fn set_music_gain(value: f32) {
    let g = value.clamp(0.0, 1.0);
    MUSIC_GAIN.set(g);
    let current = MUSIC.with(|m| m.borrow().clone());
    if let Some(path) = current {
        if let Some(sound) = lookup(&path) {
            mq::set_sound_volume(&sound, g * MASTER.get());
        }
    }
}

/// Fire a banked one-shot with the live SFX-slider gain.
pub fn play_sfx(path: &str, sfx_gain: f32) {
    match lookup(path) {
        Some(sound) => {
            // Multiplier back to pure master so successive triggers of the
            // same asset stack identically; instance volume carries the bus.
            mq::set_sound_volume(&sound, MASTER.get());
            mq::play_sound(&sound, PlaySoundParams { looped: false, volume: sfx_gain.max(0.0) });
        }
        None => eprintln!("sample sfx '{path}' not registered - skipped"),
    }
}

/// Sweep every live sampled voice to silence over `seconds` (exit fade);
/// idempotent while a fade is already running.
pub fn fade_out_all(seconds: f64) {
    if MASTER.get() <= 0.0 || FADE_RATE.get() > 0.0 {
        return; // already silent / fading out
    }
    FADE_RATE.set(1.0 / seconds.max(0.05) as f32);
}

/// Per-frame pump from the host binary: advance any exit fade, re-applying
/// the shared multipliers across the bank (music keeps its bus level).
pub fn tick(dt: f32) {
    let rate = FADE_RATE.get();
    if rate <= 0.0 || MASTER.get() <= 0.0 {
        return;
    }
    let master = (MASTER.get() - rate * dt).max(0.0);
    MASTER.set(master);

    let music = MUSIC.with(|m| m.borrow().clone());
    let gain = MUSIC_GAIN.get();
    BANK.with(|b| {
        for (path, sound) in b.borrow().iter() {
            let v = if Some(path) == music.as_ref() { gain * master } else { master };
            mq::set_sound_volume(sound, v);
        }
    });
}
