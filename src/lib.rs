//! macroquad-sion-test — a reusable RPG title-menu engine core wired to
//! libSiON-rs synthesized audio. Other projects depend on this crate and
//! write their own `#[macroquad::main]` binary; the audio engine, canvas
//! scaling, widgets and screen stack are all public.
//!
//! ## Modules
//! - [`audio`]  — single [`SiONDriver`](libsion::SiONDriver) streaming
//!   music on the main thread; SFX bank compiled from `assets/sfx/*.mml`
//!   (`sequence_on`) plus a programmatic voice path (`note_on`); music and
//!   SFX volume sliders are fully independent buses. cpal ring sink on
//!   native, JS pumped staging block in the web build.
//! - [`canvas`] — 1280x720 virtual resolution letterboxed into any window.
//! - [`config`] — asset-path resolution and `Conf` for host binaries.
//! - [`l10n`]   — locale model + `assets/i18n/*.loc` string tables
//!   (English default, falling back per key).
//! - [`scenario`] — `.scn` dialogue scripts referencing l10n keys: all
//!   in-game text is an asset, switchable live with the language option.
//! - [`ui`]     — themed draw helpers + input widgets (no audio deps).
//! - [`menu`]   — the demo screen stack: title / dialogue stub / save-slot
//!   browser / options volumes + language / fade-out exit.

pub mod audio;
pub mod canvas;
pub mod config;
pub mod l10n;
pub mod menu;
pub mod scenario;
pub mod ui;

pub mod prelude {
    pub use crate::audio::{AudioEngine, SfxId};
    pub use crate::canvas;
    pub use crate::config::{self, window_conf};
    pub use crate::l10n::{Loc, Locale};
    pub use crate::menu::{App, Outcome};
    pub use crate::scenario::Scenario;
    pub use crate::ui::widgets::FontSet;
}
