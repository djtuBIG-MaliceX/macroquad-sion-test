//! The scenario scripting contract: the shipped YAML scripts must parse
//! into the full command vocabulary, locale-suffixed variants must exist
//! and parse too, and audio paths must route to the right bus (SiMML on
//! the driver, sampled files on the macroquad `samples` bus).

use std::fs;
use std::path::Path;

use macroquad_sion_test::audio::samples;
use macroquad_sion_test::scenario::{self, Appear, Command, ImageSpec, Scenario};

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn prologue_scripts_the_full_command_vocabulary() {
    let script = Scenario::parse(&read("assets/scenario/prologue.yaml"));

    let mut saw_dialogue = false;
    let mut saw_music = false;
    let mut saw_sfx = false;
    let mut saw_image_instant_scene = false;
    let mut saw_animate_to = None;
    let mut saw_shake = false;
    let mut saw_wait = false;

    for cmd in &script.commands {
        match cmd {
            Command::Dialog { speaker, text } => {
                assert!(!speaker.is_empty() && !text.is_empty(), "dialog needs keys");
                saw_dialogue = true;
            }
            Command::Music(r) => {
                assert!(!r.path().is_empty(), "music needs a path");
                saw_music = true;
            }
            Command::Sfx(s) => {
                assert!(!s.path().is_empty() && s.beats > 0.0);
                saw_sfx = true;
            }
            Command::Image(ImageSpec::Detailed { file, appear, .. }) => {
                assert_eq!(*appear, Appear::Instant);
                saw_image_instant_scene |= scenario::is_scene(file);
            }
            Command::Image(ImageSpec::Path(p)) => {
                assert!(!p.is_empty());
            }
            Command::Animate(a) => {
                assert_eq!(a.target.as_deref(), Some("scene"), "animate targets the staged scene");
                saw_animate_to = Some(a.to);
            }
            Command::Shake(sh) => {
                assert!(sh.strength > 0.0 && sh.seconds > 0.0);
                saw_shake = true;
            }
            Command::Wait(secs) => {
                assert!(*secs > 0.0);
                saw_wait = true;
            }
            Command::Next => {}
        }
    }

    assert!(saw_dialogue && saw_music && saw_sfx, "dialog/music/sfx all scripted");
    assert!(saw_image_instant_scene, "scene image staged instantly");
    assert_eq!(saw_animate_to, Some((640.0, 150.0)), "slide destination parsed");
    assert!(saw_shake && saw_wait, "shake and waits scripted");

    let last = script.commands.last().expect("non-empty program");
    assert!(matches!(last, Command::Next), "example scenario ends with `next` (back to title)");

    // Preload bookkeeping: audio + image paths collected, `scene` excluded.
    let refs = script.asset_refs();
    assert!(refs.music.iter().any(|p| p == "assets/audio/options_theme.mml"));
    assert!(refs.effects.iter().any(|(p, beats)| p == "assets/sfx/confirm.mml" && *beats > 0.0));
    assert!(refs.images.iter().all(|p| !scenario::is_scene(p)), "scene is engine-drawn");
}

#[test]
fn locale_suffixed_scenario_parses() {
    // `<base>.<locale>.yaml` is picked by config::load_scenario at runtime;
    // here: the shipped Japanese variant must be valid on its own terms.
    let script = Scenario::parse(&read("assets/scenario/prologue.ja.yaml"));
    assert!(!script.commands.is_empty(), "ja variant stages commands");
    let last = script.commands.last().unwrap();
    assert!(matches!(last, Command::Next), "locale variant also ends with `next`");
}

#[test]
fn broken_script_degrades_to_empty_not_panic() {
    let broken = Scenario::parse("commands: [ { dialog: 42 } ]");
    assert!(broken.commands.is_empty(), "invalid YAML stays an inert empty program");
}

#[test]
fn audio_paths_split_between_buses() {
    // SiMML stays on the driver; every sampled container goes to macroquad.
    assert!(!samples::is_sample_path("assets/audio/title_theme.mml"));
    assert!(!samples::is_sample_path("assets/sfx/confirm.txt"));
    for ext in ["wav", "ogg", "mp3", "flac", "WAV", "Ogg"] {
        assert!(samples::is_sample_path(&format!("assets/audio/track.{ext}")), "{ext} must route to samples");
    }

    // Nothing is registered in this headless process...
    assert!(!samples::has("assets/audio/title_theme.mml"));
    // ...and speaking of which, a command referencing a real asset extension
    // in the shipped script still parses (bus choice happens at play time).
    let script = Scenario::parse("commands:\n  - music: assets/audio/ambient.ogg\n  - next\n");
    match &script.commands[0] {
        Command::Music(r) => assert!(samples::is_sample_path(r.path())),
        other => panic!("expected music, got {other:?}"),
    }
}
