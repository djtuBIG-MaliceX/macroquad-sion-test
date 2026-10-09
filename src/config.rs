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

    load_asset_text("assets/audio/title_theme.mml").await
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
