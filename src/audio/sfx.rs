//! Precompiled UI sound bank. Every effect is supplied as a SiMML asset
//! (`assets/sfx/<stem>.mml`, text loaded async by the host binary via
//! [`crate::config`]) and compiled once at startup; the live path only ever
//! triggers precompiled data. The slider blip stays programmatic (a pitch-
//! following `note_on` voice, not a sample/phrase) so it can track the
//! cursor continuously without re-triggering files.
//!
//! Channels come from SiON's dynamic allocator: every trigger grabs any
//! inactive slot (grown up to the driver's max track count), and disposable
//! voices self-release when their envelope ends — no fixed ids to manage.

use std::rc::Rc;

use libsion::{SiONData, SiONDriver, SiONVoice};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SfxId {
    /// Menu cursor / row change.
    Move,
    /// Enter / accept on a menu item.
    Confirm,
    /// Backspace / escape to the previous screen.
    Back,
    /// Quit-the-game phrase (played with a master fade-out).
    Exit,
}

impl SfxId {
    /// All registered effects with their asset stems (`assets/sfx/<stem>.mml`).
    pub const ALL: &'static [(SfxId, &'static str)] = &[
        (SfxId::Move, "move"),
        (SfxId::Confirm, "confirm"),
        (SfxId::Back, "back"),
        (SfxId::Exit, "exit"),
    ];

    pub fn asset_path(self) -> String {
        let stem = Self::ALL
            .iter()
            .find(|(id, _)| *id == self)
            .map(|(_, stem)| *stem)
            .unwrap_or("unknown");
        format!("assets/sfx/{stem}.mml")
    }

    /// Upper bound on the one-shot phrase (16th-beats) handed to
    /// `sequence_on`; assets may be shorter but not effectively longer.
    pub(crate) fn max_length(self) -> f64 {
        match self {
            SfxId::Move => 2.0,
            SfxId::Confirm | SfxId::Back => 24.0,
            SfxId::Exit => 64.0,
        }
    }
}

pub struct SfxBank {
    entries: Vec<(SfxId, Rc<std::cell::RefCell<SiONData>>, f64)>,
    blip: SiONVoice,
}

impl SfxBank {
    /// Compile every supplied one-shot exactly once. Missing ids simply
    /// never sound (loader logs the asset error). Must run *before* `play`
    /// (`compile()` stops the live stream). Sources come from asset files,
    /// e.g. `config::load_asset_text(&id.asset_path())`.
    pub fn precompile(driver: &mut SiONDriver, sources: &[(SfxId, String)]) -> Self {
        let entries = sources
            .iter()
            .map(|(id, mml)| (*id, driver.compile(mml.clone()), id.max_length()))
            .collect();

        let mut blip = SiONVoice::new(libsion::sion_enums::MODULE_PULSE, 2, 63, 0, 0, -1, 0, 0);
        blip.set_envelope(96, 40, 0, 127, 96, 96);
        blip.set_name("ui-blip".to_string());

        SfxBank { entries, blip }
    }

    /// Drop all arena-backed compiled data (called from `AudioEngine::Drop`
    /// before `sion::finalize()`).
    pub fn clear_compiled(&mut self) {
        self.entries.clear();
    }

    /// Map an asset path back to its bank id, so scenario `sfx:` commands
    /// that point at a bank asset reuse the precompiled entry instead of
    /// demanding a duplicate registration.
    pub fn id_for_path(&self, path: &str) -> Option<SfxId> {
        self.entries.iter().find(|(id, _, _)| id.asset_path() == path).map(|(id, _, _)| *id)
    }

    pub fn play(
        &self,
        driver: &mut SiONDriver,
        id: SfxId,
        sfx_gain: f32,
    ) {
        if !driver.is_streaming() {
            return;
        }
        for (entry_id, data, length) in &self.entries {
            if *entry_id != id {
                continue;
            }
            // Quantize/delay stay 0 (fire now); -1 = "any free channel".
            let tracks = driver.sequence_on(data, None, *length, 0.0, 0.0, -1, true);
            apply_gain(&tracks, sfx_gain);
        }
    }

    /// Programmatic pitch-following click for sliders (0.0..=1.0).
    ///
    /// Track id must be `0`: `note_on` swaps arguments internally (a quirk
    /// faithfully ported from C++) and repurposes the id as the key-on
    /// quantize grid — non-zero ids delay the blip by whole beats.
    pub fn slider_blip(&self, driver: &mut SiONDriver, norm: f32, sfx_gain: f32) {
        if !driver.is_streaming() {
            return;
        }
        let norm = norm.clamp(0.0, 1.0);
        let note = 57 + (norm * 24.0).round() as i32; // A3..A5
        if let Some(track) = driver.note_on(note, Some(&self.blip), 2.0, 0.0, 0.0, 0, true) {
            apply_gain(&[track], sfx_gain);
        }
    }
}

/// Scale the just-created disposable track(s) by the SFX slider. Reading
/// back the default master volume keeps the bank timbre-agnostic. The music
/// slider lives on MML tracks' own master volumes (`AudioEngine`), so this
/// chain never moves with it — quiet music + loud SFX works.
fn apply_gain(tracks: &[Rc<std::cell::RefCell<libsion::SiMMLTrack>>], sfx_gain: f32) {
    for track in tracks {
        let mut tb = track.borrow_mut();
        let default_master = tb.get_master_volume().max(1) as f32;
        tb.set_master_volume((default_master * sfx_gain) as i32);
    }
}
