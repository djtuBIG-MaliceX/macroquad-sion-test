//! Window config and asset-path resolution shared by every layer.
//!
//! Assets use one path convention on both targets: `"assets/..."` relative
//! to the working directory. Native runs from the repo root (`cargo run`);
//! the web layout serves `web/assets/...` over HTTP via
//! [`macroquad::file`] (XHR through the miniquad glue), copied there by
//! `web/build-wasm.ps1`. `$SION_TEST_ASSETS` overrides with a filesystem
//! root on native for quick experimentation.

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use macroquad::prelude::*;

pub const WINDOW_WIDTH: u32 = 1280;
pub const WINDOW_HEIGHT: u32 = 720;

/// Default (English) title-theme registration key and asset location.
pub const TITLE_THEME_PATH: &str = "assets/audio/title_theme.mml";
/// Options-screen theme; keys double as registration paths in the engine.
pub const OPTIONS_THEME_PATH: &str = "assets/audio/options_theme.mml";

/// Passed to `#[macroquad::main(...)]` in downstream binaries.
pub fn window_conf() -> Conf {
    Conf {
        window_title: "macroquad-sion-test — Title Menu".to_string(),
        window_width: WINDOW_WIDTH as i32,
        window_height: WINDOW_HEIGHT as i32,
        // the 16:9 virtual canvas rescales itself via `canvas::end_frame`
        window_resizable: true,
        ..Default::default()
    }
}

/// Asset bytes via macroquad's cross-platform async loader (HTTP on wasm,
/// std::fs on native). Falls back to `$SION_TEST_ASSETS/<rel>` on failure.
pub async fn load_asset_bytes(rel: &str) -> Result<Vec<u8>, String> {
    match macroquad::file::load_file(rel).await {
        Ok(bytes) => Ok(bytes),
        Err(file_err) => {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(root) = std::env::var_os("SION_TEST_ASSETS") {
                let path: PathBuf = [Path::new(&root), Path::new(rel)].iter().collect();
                match std::fs::read(&path) {
                    Ok(bytes) => return Ok(bytes),
                    Err(e) => return Err(format!("cannot read {} ({e})", path.display())),
                }
            }

            Err(format!("cannot load asset '{rel}' ({file_err:?})"))
        }
    }
}

pub async fn load_asset_text(rel: &str) -> Result<String, String> {
    let bytes = load_asset_bytes(rel).await?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

/// Title-theme MML; `$SION_TEST_MML` overrides with a direct file path
/// (native convenience for swapping tunes without touching assets/).
pub async fn title_mml() -> Result<String, String> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = std::env::var_os("SION_TEST_MML") {
        let path = PathBuf::from(path);
        return std::fs::read_to_string(&path)
            .map(|text| text.trim_start_matches('\u{feff}').to_string())
            .map_err(|e| format!("cannot read MML '{}' ({e})", path.display()));
    }

    load_asset_text(TITLE_THEME_PATH).await
}

/// Load one audio asset and classify it by flavour: SiMML text for MML-ish
/// extensions, raw container bytes (decoded later by the `samples` bus via
/// macroquad — wav/ogg/mp3…) for everything else.
pub async fn load_audio_asset(path: &str) -> Result<crate::audio::AudioAsset, String> {
    if crate::audio::samples::is_sample_path(path) {
        Ok(crate::audio::AudioAsset::Bytes(load_asset_bytes(path).await?))
    } else {
        Ok(crate::audio::AudioAsset::Mml(load_asset_text(path).await?))
    }
}

/// Locale-aware scenario resolution by filename suffix: try
/// `assets/scenario/<base>.<locale>.yaml` first, then the default
/// (English) `assets/scenario/<base>.yaml`. Returns the parsed script and
/// which file produced it (empty when only the embedded fallback ran).
pub async fn load_scenario(
    base: &str,
    locale: crate::l10n::Locale,
) -> Result<(crate::scenario::Scenario, String), String> {
    let stem = locale.stem();
    let localized = format!("assets/scenario/{base}.{stem}.yaml");
    let mut tried = Vec::new();

    for path in [localized.as_str(), &format!("assets/scenario/{base}.yaml")] {
        match load_asset_text(path).await {
            Ok(text) => return Ok((crate::scenario::Scenario::parse(&text), path.to_string())),
            Err(e) => tried.push(format!("{path} ({e})")),
        }
    }
    Err(format!("no scenario asset for '{base}': {}", tried.join(", ")))
}

/// Fetch + decode every referenced PNG once at startup into a path-keyed
/// bank (scenario `image:` commands then draw without async). Native path
/// resolution follows macroquad's cwd convention; `$SION_TEST_ASSETS` does
/// not apply to textures. Failures are skipped with a warning — the stage
/// simply never shows that layer.
pub async fn load_image_assets(paths: &[String]) -> std::collections::HashMap<String, Texture2D> {
    let mut bank = std::collections::HashMap::new();
    for path in paths {
        match macroquad::texture::load_texture(path).await {
            Ok(tex) => {
                bank.insert(path.clone(), tex);
            }
            Err(e) => eprintln!("image asset '{path}' unavailable: {e}"),
        }
    }
    bank
}

/// Load every registered SFX one-shot from `assets/sfx/<stem>.mml`.
/// Effects whose asset fails to load are omitted from the result (a warning
/// is printed); the bank then simply never sounds for that id.
pub async fn sfx_sources() -> Vec<(crate::audio::SfxId, String)> {
    let mut sources = Vec::new();
    for (id, _stem) in crate::audio::SfxId::ALL {
        let path = id.asset_path();
        match load_asset_text(&path).await {
            Ok(text) => sources.push((*id, text.trim().to_string())),
            Err(e) => eprintln!("sfx asset '{path}' unavailable: {e}"),
        }
    }
    sources
}
