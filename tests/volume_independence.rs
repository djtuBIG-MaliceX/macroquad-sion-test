//! Integration proof for the two headline audio requirements:
//! music and SFX volume are separate buses, and SFX live as assets.

use std::fs;
use std::path::Path;

use macroquad_sion_test::audio::{AudioEngine, SfxId};

fn read(rel: &str) -> String {
    // integration tests run with cwd = crate root.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn music_gain_is_independent_and_never_compounds() {
    let mml = read("assets/audio/title_theme.mml");

    // SFX supplied purely from assets/sfx/*.mml (the runtime contract).
    let sfx: Vec<(SfxId, String)> = SfxId::ALL
        .iter()
        .map(|(id, _stem)| (*id, read(&id.asset_path()).trim().to_string()))
        .collect();
    assert_eq!(sfx.len(), SfxId::ALL.len());

    let mut engine = AudioEngine::new(&sfx);
    // Registration keys are asset paths; start_music plays the first one.
    engine.add_song(macroquad_sion_test::config::TITLE_THEME_PATH, mml);
    engine.set_music_volume(1.0);
    engine.start_music();

    // One processed block materialises the parsed song tracks.
    engine.pump_for_test(3);
    let full = engine.music_track_master_volumes();
    assert!(!full.is_empty(), "no MML music tracks materialised");
    let any_audio = full.iter().any(|&v| v > 0);
    assert!(any_audio, "song tracks authored silent at unity");

    // Drop music to a quarter; every song track scales once…
    engine.set_music_volume(0.25);
    engine.pump_for_test(1);
    let quarter = engine.music_track_master_volumes();

    // …and keeps scaling the SAME amount for many further blocks (no
    // compounding toward silence — each stays near authored*0.25, not
    // authored*0.25^n).
    engine.pump_for_test(30);
    let after = engine.music_track_master_volumes();
    assert_eq!(quarter, after, "music baseline compounding across blocks");

    for (slot, (&a, &q)) in full.iter().zip(quarter.iter()).enumerate() {
        if a == 0 {
            assert_eq!(q, 0, "silent track #{slot} changed");
        } else {
            let expected = (a as f32 * 0.25).round() as i32;
            assert!(
                (q - expected).abs() <= 1,
                "track #{slot}: authored {a} @0.25 -> got {q}, expected ~{expected}"
            );
        }
    }

    // Firing SFX at full sfx_gain must NOT move any music track off its
    // slider level (separate DRIVER_SEQUENCE tracks).
    let before_sfx = engine.music_track_master_volumes();
    engine.play_sfx(SfxId::Confirm, 1.0);
    engine.slider_blip(0.6, 1.0);
    let extra_before = engine.total_track_count();
    assert!(extra_before >= before_sfx.len() as i32);

    engine.pump_for_test(4);
    let after_sfx = engine.music_track_master_volumes();
    assert_eq!(before_sfx, after_sfx, "SFX triggers disturbed the music bus");
}

#[test]
fn sfx_assets_are_the_single_source_of_truth() {
    // Each registered id has a real asset file on disk (native repo).
    for (id, _stem) in SfxId::ALL {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(id.asset_path());
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing sfx asset {}: {e}", path.display()));
        let notes = text.trim();
        assert!(!notes.is_empty(), "empty sfx asset {}", path.display());
        // Must be real SiMML (tempo + at least a note or rest command).
        assert!(notes.contains('t') && notes.ends_with(';'), "not one-shot SiMML: {notes:?}");
    }

    // An engine with no SFX sources still boots (the demo simply loses that
    // effect rather than crashing) — assets are optional, not duplicated.
    let mml = read("assets/audio/title_theme.mml");
    let mut engine = AudioEngine::new(&[]);
    engine.add_song(macroquad_sion_test::config::TITLE_THEME_PATH, mml);
    engine.start_music();
    engine.pump_for_test(2);
    assert!(engine.is_streaming());
    assert!(!engine.music_track_master_volumes().is_empty());

    // Triggering a bank with no compiled entries is an inert no-op.
    for (id, _stem) in SfxId::ALL {
        engine.play_sfx(*id, 1.0);
    }
}
