//! macroquad-sion-test — a reusable RPG title-menu engine core wired to
//! libSiON-rs synthesized audio. Other projects depend on this crate and
//! write their own `#[macroquad::main]` binary; the audio engine, canvas
//! scaling, widgets and screen stack are all public.
//!
//! ## Modules
//! - [`audio`]  — single [`SiONDriver`](libsion::SiONDriver) streaming
//!   music on the main thread; songs registered by asset path (one theme
//!   per screen, every `play_music` restarts from the top), SFX bank
//!   compiled from `assets/sfx/*.mml` (`sequence_on`) plus scenario-file
//!   one-shots. Sampled assets (`.wav`/`.ogg` everywhere, `.mp3` in the
//!   browser) play through `macroquad::audio` on a sibling bus ([`audio::samples`]).
//!   Music and SFX volume sliders are fully independent buses. cpal ring
//!   sink on native, JS pumped staging block in the web build.
//! - [`canvas`] — 1280x720 virtual resolution letterboxed into any window,
//!   plus an opt-in screen-shake offset for scripted staging.
//! - [`config`] — asset-path resolution (locale-suffixed scenario files
//!   first, English default) and `Conf` for host binaries.
//! - [`l10n`]   — locale model + `assets/i18n/*.loc` string tables
//!   (English default, falling back per key).
//! - [`scenario`] — YAML command scripts (`assets/scenario/*.yaml`):
//!   dialogue, music/SFX changes (SiMML or sampled files), staged images with fade
//!   instant appears, animation, screen shake, waits and `next`. In-game
//!   text stays l10n keys, switchable live with the language option.
//! - [`ui`]     — themed draw helpers + input widgets (no audio deps).
//! - [`menu`]   — the demo screen stack: title / scripted scene runner /
//!   save-slot browser / options volumes + language / fade-out exit.

pub mod audio;
pub mod canvas;
pub mod config;
pub mod l10n;
pub mod menu;
pub mod scenario;
pub mod ui;

pub mod prelude {
    pub use crate::audio::{AudioAsset, AudioEngine, SfxId};
    pub use crate::canvas;
    pub use crate::config::{self, window_conf};
    pub use crate::l10n::{Loc, Locale};
    pub use crate::menu::{App, Outcome};
    pub use crate::scenario::{self, Scenario};
    pub use crate::ui::widgets::FontSet;
}
