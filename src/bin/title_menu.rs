//! Native + web demo binary: boots the audio engine, installs it on the
//! thread-local shared with browser JS pumping, and runs the title-menu
//! screen stack. Downstream games replace `main` with their own loop and
//! reuse the engine modules instead.

use macroquad::prelude::*;

use macroquad_sion_test::audio::{self, samples, AudioAsset, AudioEngine, SfxId};
use macroquad_sion_test::canvas;
use macroquad_sion_test::config::{self, window_conf};
use macroquad_sion_test::l10n::{Loc, Locale};
use macroquad_sion_test::menu::{App, Outcome};
use macroquad_sion_test::scenario::Scenario;
use macroquad_sion_test::ui::widgets::FontSet;

#[cfg(not(target_arch = "wasm32"))]
const KEEP_AUDIO_FRAMES: usize = 4096;

#[macroquad::main(window_conf)]
async fn main() {
    // A missing title-theme asset means silence: no built-in tune is ever
    // played in its place.
    let title_mml = match config::title_mml().await {
        Ok(text) => Some(text),
        Err(e) => {
            eprintln!("title theme unavailable: {e} - music disabled");
            None
        }
    };

    let font_bytes = config::load_asset_bytes("assets/fonts/NotoSansJP-VF.ttf")
        .await
        .ok();
    let fonts = FontSet::from_bytes(font_bytes.as_deref(), "NotoSansJP-VF.ttf");

    // Text is asset-driven: embedded tables keep the demo whole, fetched
    // assets override them (hot-swap strings without recompiling). All UI
    // and scripted dialogue flows through these; English is the default.
    let mut loc = Loc::with_defaults();
    for locale in Locale::ALL {
        let path = format!("assets/i18n/{}.loc", locale.stem());
        match config::load_asset_text(&path).await {
            Ok(text) => loc.load(locale, &text),
            Err(e) => eprintln!("l10n asset '{path}' unavailable: {e}"),
        }
    }

    // Scenario program, locale-resolved (`<base>.<locale>.yaml` first:
    // Japanese staging when selected, this English default otherwise). Its
    // referenced audio/images must ALL be preloaded now: SiMML compilation
    // stops the live stream, so registration happens before any playback.
    let base = "prologue";
    let (script, scn_path) = match config::load_scenario(base, loc.active()).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("scenario assets unavailable: {e} - using embedded prologue");
            (Scenario::prologue(), String::new())
        }
    };
    if !scn_path.is_empty() {
        eprintln!("scenario script: {scn_path}");
    }

    // SFX bank is asset-driven: load every assets/sfx/<stem>.mml before the
    // engine compiles them. Missing files are skipped with a warning by the
    // loader.
    let sfx_sources = config::sfx_sources().await;

    // Screen-music paths: options theme plus anything extra the script's
    // `music:` commands reference (deduplicated).
    let refs = script.asset_refs();
    let mut music_paths: Vec<String> = vec![config::OPTIONS_THEME_PATH.to_string()];
    for p in refs.music {
        if !music_paths.iter().any(|m| m == &p) {
            music_paths.push(p);
        }
    }

    // mut only needed natively for cpal stream wiring (web pumps via JS)
    #[allow(unused_mut)]
    let mut engine = AudioEngine::new(&sfx_sources);

    if let Some(mml) = title_mml {
        engine.add_song(config::TITLE_THEME_PATH, mml);
    }
    for path in &music_paths {
        match config::load_audio_asset(path).await {
            Ok(AudioAsset::Mml(mml)) => engine.add_song(path, mml),
            // sampled files (wav/ogg/mp3…) decode via macroquad's `samples` bus
            Ok(AudioAsset::Bytes(bytes)) => samples::register(path, &bytes).await,
            Err(e) => eprintln!("music asset '{path}' unavailable: {e}"),
        }
    }

    // Scenario `sfx:` assets that aren't already bank effects (bank ids
    // keep their own precompiled entries and route by path at play time).
    for (path, beats) in refs.effects {
        if SfxId::ALL.iter().any(|(id, _)| id.asset_path() == path) {
            continue;
        }
        match config::load_audio_asset(&path).await {
            Ok(AudioAsset::Mml(mml)) => engine.add_effect_mml(&path, mml, beats),
            Ok(AudioAsset::Bytes(bytes)) => samples::register(&path, &bytes).await,
            Err(e) => eprintln!("sfx asset '{path}' unavailable: {e}"),
        }
    }

    let images = config::load_image_assets(&refs.images).await;

    #[cfg(not(target_arch = "wasm32"))]
    let _stream = match engine.start_native_output() {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("audio output failed: {e}");
            std::process::exit(1);
        }
    };

    audio::install(engine);
    audio::with(|e| e.start_music());

    let mut app = App::new(fonts, loc, script, images);

    let autoexit: Option<f64> = std::env::var("SION_TEST_AUTOEXIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok());
    let auto_sfx_ms: Option<u64> = std::env::var("SION_TEST_SFX_AUTO_MS")
        .ok()
        .and_then(|v| v.parse().ok());
    let mut sfx_tick = 0u64;
    // miniquad monotonic clock: works on native and browser alike (std
    // Instant is a runtime trap on wasm32-unknown-unknown)
    let started = get_time();

    loop {
        let dt = get_frame_time();
        let now_s = get_time() - started;

        // Locale swap in Options => fetch the `<base>.<locale>` scenario and
        // anything new it references. Compiling fresh MML momentarily stops
        // the stream; the loop pump resyncs whichever song owns `current`,
        // so music simply starts again over the new staging.
        if let Some(base) = app.take_scenario_request() {
            let locale = app.loc.active();
            let (script, path) = match config::load_scenario(&base, locale).await {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("scenario reload '{base}' failed: {e} - embedded fallback");
                    let embedded = match locale {
                        Locale::Japanese => Scenario::prologue_ja(),
                        Locale::English => Scenario::prologue(),
                    };
                    (embedded, String::new())
                }
            };
            if !path.is_empty() {
                eprintln!("scenario script: {path}");
            }

            let refs = script.asset_refs();
            for path in &refs.music {
                // sample assets live in the macroquad bank; MML songs on the driver
                let known = if samples::is_sample_path(path) {
                    samples::has(path)
                } else {
                    audio::with(|e| e.has_track(path)).unwrap_or(false)
                };
                if known {
                    continue;
                }
                match config::load_audio_asset(path).await {
                    Ok(AudioAsset::Mml(mml)) => drop(audio::with(|e| e.add_song(path, mml))),
                    Ok(AudioAsset::Bytes(bytes)) => samples::register(path, &bytes).await,
                    Err(e) => eprintln!("music asset '{path}' unavailable: {e}"),
                }
            }
            for (path, beats) in &refs.effects {
                if SfxId::ALL.iter().any(|(id, _)| id.asset_path() == *path) {
                    continue; // bank effect already registered
                }
                match config::load_audio_asset(path).await {
                    Ok(AudioAsset::Mml(mml)) => drop(audio::with(|e| e.add_effect_mml(path, mml, *beats))),
                    Ok(AudioAsset::Bytes(bytes)) => samples::register(path, &bytes).await,
                    Err(e) => eprintln!("sfx asset '{path}' unavailable: {e}"),
                }
            }
            app.add_images(config::load_image_assets(&refs.images).await);
            app.set_scenario(&base, script);
        }

        #[cfg(not(target_arch = "wasm32"))]
        audio::with(|e| e.tick_native(KEEP_AUDIO_FRAMES));

        // macroquad sample-bus housekeeping (exit-fade ramp; no-op at rest)
        audio::tick(dt);

        if let Some(ms) = auto_sfx_ms {
            let t = (now_s * 1000.0) as u64;
            if t / ms > sfx_tick {
                sfx_tick = t / ms;
                let id = match sfx_tick % 3 {
                    0 => macroquad_sion_test::audio::SfxId::Move,
                    1 => macroquad_sion_test::audio::SfxId::Confirm,
                    _ => macroquad_sion_test::audio::SfxId::Back,
                };
                eprintln!("[auto-sfx] fire {sfx_tick} ({id:?})");
                audio::with(|e| {
                    e.play_sfx(id, 0.8);
                    if sfx_tick % 5 == 4 {
                        e.slider_blip(0.3 + (sfx_tick as f32 * 0.1).fract(), 0.8);
                    }
                });
            }
        }

        match app.update(dt) {
            Outcome::Continue => {}
            Outcome::Quit => break,
        }

                        clear_background(BLACK); // letterbox bars stay black behind the canvas rect
        canvas::begin_frame();
        app.draw();
        if std::env::var("SION_DEBUG_FLIP").is_ok() {
            // orientation probe drawn last so nothing overwrites it:
            // yellow L in top-left, red top-right corner, blue bottom-left
            draw_rectangle(20.0, 60.0, 30.0, 120.0, YELLOW);
            draw_rectangle(20.0, 150.0, 130.0, 30.0, YELLOW);
            draw_rectangle(canvas::vw() - 82.0, 20.0, 60.0, 60.0, RED);
            draw_rectangle(20.0, canvas::vh() - 80.0, 60.0, 60.0, BLUE);

            // A/B text channel probe:
            //   "A" = FontSet pipeline (mirrored cam)   "B" = normal pass camera
            app.fonts.text("A456", 120.0, 90.0, 70.0, GREEN);
            canvas::set_pass_camera();
            draw_text_ex(
                "B123",
                430.0,
                90.0,
                TextParams {
                    font: app.fonts.regular.as_ref(),
                    font_size: 70,
                    color: PURPLE,
                    ..Default::default()
                },
            );
        }
        canvas::end_frame();
        if let Some(secs) = autoexit {
            if now_s >= secs {
                eprintln!("smoke-test window: auto-exit after {secs}s");
                break;
            }
        }

        next_frame().await;
    }

    // graceful teardown: sweep both buses (driver + macroquad samples), let
    // queued samples drain, stop the callback and only then release the
    // global SiON statics.
    audio::fade_all_out(0.4);

    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(e) = audio::take() {
            e.drain_native_output();
            drop(_stream);
            drop(e);
        } else {
            std::mem::drop(_stream);
        }
        libsion::sion::finalize();
    }

    #[cfg(target_arch = "wasm32")]
    { /* browser: leave statics alone; JS tears the audio context down */ }
}

// ---------------------------------------------------------------------------
// web glue: JS audio callbacks pump `frames` frames of interleaved stereo
// f32 straight out of the engine's staging block (see web/index.html).
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn sion_wasm_pump(frames: u32) -> *const f32 {
    // one call returns at most one driver block; the glue asks for exactly
    // BLOCK_FRAMES (2048) at a 44100 Hz AudioContext
    let frames = (frames as usize).clamp(1, audio::BLOCK_FRAMES);
    audio::with(|e| e.pump_web(frames)).unwrap_or(std::ptr::null())
}

#[cfg(not(target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn sion_wasm_pump(_frames: u32) -> *const f32 {
    std::ptr::null()
}
