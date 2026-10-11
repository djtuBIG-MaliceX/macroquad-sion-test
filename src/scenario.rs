//! Scenario scripts: in-game staging is data, not code. A `.yaml` script
//! is an ordered command list the [`crate::menu`] scene runner executes —
//! dialogue, music/SFX swaps (SiMML on the driver, .ogg/.wav/.mp3 via macroquad), images with fade/instant
//! appears, image animations, screen shake, waits and `next` to end.
//!
//! Parsing is deliberately hand-rolled over [`yaml_rust2::Yaml`] values:
//! the command vocabulary is small and fixed, error reporting stays
//! readable, and it sidesteps the archived serde_yaml ecosystem's breakage
//! on enum maps under current toolchains.
//!
//! Locale selection lives in [`crate::config::load_scenario`]: a script
//! base name resolves to `<base>.<locale>.yaml` first, then the default
//! (English) `<base>.yaml`. Dialogue `speaker`/`text` are literal copy
//! carried by each locale's YAML (swap the whole script file to switch
//! language), so no `.loc` keys are involved — the runner reads the text
//! straight out of [`Command::Dialog`].

use yaml_rust2::{Yaml, YamlLoader};

/// Parse entry point: the whole command program of one script file.
#[derive(Debug)]
pub struct Scenario {
    pub commands: Vec<Command>,
}

/// One scripted staging step. YAML form: the command name is the mapping
/// key, e.g. `- dialog: {speaker: x, text: y}` or bare `- next`.
#[derive(Debug, Clone)]
pub enum Command {
    /// Stage a line in the dialogue box; execution pauses until the
    /// player advances (both fields are literal text from the YAML).
    Dialog { speaker: String, text: String },

    /// Switch the looping music track (restart from the top).
    Music(MusicSpec),

    /// Fire a one-shot effect. `beats` bounds the MML sequence window; a
    /// `.mid` cue instead plays its whole length on reserved tracks, mixed in
    /// at the SFX level while the current music keeps playing.
    Sfx(SfxSpec),

    /// Add a layer to the stage (backgrounds first is the convention).
    Image(ImageSpec),

    /// Slide a staged image to new top-left coordinates.
    Animate(AnimateSpec),

    /// Camera shake in virtual pixels, decaying over `seconds`.
    Shake(ShakeSpec),

    /// Silent delay in seconds.
    Wait(f32),

    /// End the scenario (the demo returns to the title menu).
    Next,
}

/// `music:` value: path scalar or `{file}` map.
#[derive(Debug, Clone)]
pub struct MusicSpec {
    pub file: String,
}

impl MusicSpec {
    pub fn path(&self) -> &str {
        &self.file
    }
}

/// `sfx:` value: path scalar or `{file, beats}` map. `beats` bounds the
/// MML sequence window (16th-note beats); irrelevant for sampled SFX.
#[derive(Debug, Clone)]
pub struct SfxSpec {
    pub file: String,
    pub beats: f64,
}

impl SfxSpec {
    pub fn path(&self) -> &str {
        &self.file
    }
}

/// Appear style for staged images.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Appear {
    #[default]
    Fade,
    Instant,
}

/// `image:` value: path scalar (fade in, centered) or a detailed staging
/// box. `file: scene` names the engine-generated placeholder backdrop; any
/// other value is a banked PNG path. `name` names the layer for later
/// `animate:` targets (defaults to the file stem).
#[derive(Debug, Clone)]
pub enum ImageSpec {
    Path(String),
    Detailed {
        file: String,
        name: Option<String>,
        appear: Appear,
        seconds: f32,
        x: Option<f32>,
        y: Option<f32>,
        w: Option<f32>,
        h: Option<f32>,
    },
}

impl ImageSpec {
    pub fn path(&self) -> &str {
        match self {
            ImageSpec::Path(p) | ImageSpec::Detailed { file: p, .. } => p,
        }
    }
}

/// `animate:` value: `{target?, to: [x, y], seconds}`; target defaults to
/// the most recently staged image.
#[derive(Debug, Clone)]
pub struct AnimateSpec {
    pub target: Option<String>,
    /// Destination top-left corner in virtual pixels.
    pub to: (f32, f32),
    pub seconds: f32,
}

/// `shake:` value: scalar seconds (default strength) or `{strength, seconds}`.
#[derive(Debug, Clone)]
pub struct ShakeSpec {
    pub strength: f32,
    pub seconds: f32,
}

const DEFAULT_SHAKE_STRENGTH: f32 = 10.0;
const DEFAULT_SFX_BEATS: f64 = 8.0;

/// Everything a script pre-references by path, for startup preloading:
/// the engine must register every audio asset before `play_music` and the
/// host must fetch textures async, both impossible mid-command.
#[derive(Debug, Default)]
pub struct ScenarioAssets {
    /// Music/audio-file paths (`music:` commands).
    pub music: Vec<String>,
    /// (path, sequence beats) per `sfx:` command, deduplicated.
    pub effects: Vec<(String, f64)>,
    /// PNG paths per `image:` command (`scene` excluded — engine-drawn).
    pub images: Vec<String>,
}

const SCENE_KEYWORD: &str = "scene";

impl Scenario {
    /// Parse the YAML command program. Malformed entries are reported to
    /// stderr and skipped (the rest of the script still runs); a completely
    /// invalid file yields an empty program, never a panic.
    pub fn parse(text: &str) -> Scenario {
        let docs = match YamlLoader::load_from_str(text) {
            Ok(docs) => docs,
            Err(e) => {
                eprintln!("scenario script invalid: {e}");
                return Scenario { commands: Vec::new() };
            }
        };
        let Some(root) = docs.first() else {
            return Scenario { commands: Vec::new() };
        };

        let Some(items) = child(root, "commands").and_then(Yaml::as_vec) else {
            eprintln!("scenario script has no top-level `commands:` list");
            return Scenario { commands: Vec::new() };
        };

        let mut commands = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            match parse_command(item) {
                Ok(cmd) => commands.push(cmd),
                Err(why) => eprintln!("scenario command #{i} skipped: {why}"),
            }
        }
        Scenario { commands }
    }

    /// Collect every distinct audio/image asset path the program references.
    pub fn asset_refs(&self) -> ScenarioAssets {
        let mut out = ScenarioAssets::default();
        for cmd in &self.commands {
            match cmd {
                Command::Music(m) => push_unique(&mut out.music, m.path()),
                Command::Sfx(s) => {
                    if !out.effects.iter().any(|(p, _)| p == s.path()) {
                        out.effects.push((s.path().to_string(), s.beats));
                    }
                }
                Command::Image(spec) => {
                    let p = spec.path();
                    if p != SCENE_KEYWORD {
                        push_unique(&mut out.images, p);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The shipped prologue, embedded so the demo runs without asset I/O.
    pub fn prologue() -> Scenario {
        Self::parse(include_str!("../assets/scenario/prologue.yaml"))
    }

    /// The Japanese-locale prologue variant (demonstrates `<base>.<locale>`
    /// filename i18n; kept embedded like the default table).
    pub fn prologue_ja() -> Scenario {
        Self::parse(include_str!("../assets/scenario/prologue.ja.yaml"))
    }
}

fn parse_command(item: &Yaml) -> Result<Command, String> {
    // Bare scalar forms (only `next` is meaningful as a lone token).
    if let Some(word) = item.as_str() {
        return match word.trim() {
            "next" => Ok(Command::Next),
            other => Err(format!("unknown bare command '{other}'")),
        };
    }

    // Commands are single-key maps: `- <name>: <value>`.
    let pairs = match item {
        Yaml::Hash(map) => map.iter().collect::<Vec<_>>(),
        _ => return Err("expected a `command: value` mapping".into()),
    };
    let (k, value) = *pairs.first().ok_or("empty command mapping")?;
    let cmd_name = k.as_str().ok_or("command name must be a string")?;

    match cmd_name {
        "dialog" => {
            let speaker = req_str(value, "speaker")?;
            let text = req_str(value, "text")?;
            Ok(Command::Dialog { speaker, text })
        }
        "music" => Ok(Command::Music(MusicSpec { file: asset_path(value, "music")? })),
        "sfx" => {
            let file = asset_path(value, "sfx")?;
            let beats =
                opt_f32(value, "beats").map(|b| b.max(1.0) as f64).unwrap_or(DEFAULT_SFX_BEATS);
            Ok(Command::Sfx(SfxSpec { file, beats }))
        }
        "image" => Ok(Command::Image(parse_image(value)?)),
        "animate" => {
            let to = child(value, "to")
                .and_then(Yaml::as_vec)
                .filter(|v| v.len() >= 2)
                .and_then(|v| Some((num_f32(&v[0])?, num_f32(&v[1])?)))
                .ok_or("`animate` needs `to: [x, y]`")?;
            Ok(Command::Animate(AnimateSpec {
                target: opt_str(value, "target"),
                to,
                seconds: opt_f32(value, "seconds").unwrap_or(1.0).max(0.05),
            }))
        }
        "shake" => {
            // scalar == seconds at default strength; map for full control.
            if let Some(secs) = num_f32(value) {
                Ok(Command::Shake(ShakeSpec {
                    strength: DEFAULT_SHAKE_STRENGTH,
                    seconds: secs.max(0.05),
                }))
            } else {
                Ok(Command::Shake(ShakeSpec {
                    strength: opt_f32(value, "strength").unwrap_or(DEFAULT_SHAKE_STRENGTH),
                    seconds: req_num(value, "seconds")?.max(0.05),
                }))
            }
        }
        "wait" => {
            let secs = num_f32(value).ok_or("`wait` needs a number (seconds)")?;
            Ok(Command::Wait(secs.max(0.0)))
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

/// `image:` body: scalar path, or `{file, name?, appear?, seconds?, x?, y?, w?, h?}`.
fn parse_image(value: &Yaml) -> Result<ImageSpec, String> {
    if let Some(p) = value.as_str() {
        return Ok(ImageSpec::Path(p.to_string()));
    }
    let file = req_str(value, "file").map_err(|_| "image needs `file` (or a bare path)")?;
    let appear = match opt_str(value, "appear").as_deref() {
        Some("instant") => Appear::Instant,
        _ => Appear::Fade,
    };
    Ok(ImageSpec::Detailed {
        file,
        name: opt_str(value, "name"),
        appear,
        seconds: opt_f32(value, "seconds").unwrap_or(0.5).max(0.01),
        x: opt_f32(value, "x"),
        y: opt_f32(value, "y"),
        w: opt_f32(value, "w"),
        h: opt_f32(value, "h"),
    })
}

/// Music/SFX asset value: bare path scalar or `{file: ...}` map.
fn asset_path(value: &Yaml, what: &str) -> Result<String, String> {
    if let Some(p) = value.as_str() {
        return Ok(p.to_string());
    }
    opt_str(value, "file").ok_or_else(|| format!("{what} needs a path or `file:`"))
}

// --- Yaml value conveniences -------------------------------------------------

fn num_f32(y: &Yaml) -> Option<f32> {
    y.as_i64().map(|v| v as f32).or_else(|| y.as_f64().map(|v| v as f32))
}

/// Borrow a mapping value by string key without relying on indexing traits,
/// so minor yaml-rust2 API shifts can't bite us.
fn child<'a>(v: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    match v {
        Yaml::Hash(map) => map.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, val)| val),
        _ => None,
    }
}

fn opt_str(v: &Yaml, key: &str) -> Option<String> {
    child(v, key).and_then(Yaml::as_str).map(str::to_string)
}

fn req_str(v: &Yaml, key: &str) -> Result<String, String> {
    opt_str(v, key).filter(|s| !s.is_empty()).ok_or_else(|| format!("missing required `{key}`"))
}

fn opt_f32(v: &Yaml, key: &str) -> Option<f32> {
    child(v, key).and_then(num_f32)
}

fn req_num(v: &Yaml, key: &str) -> Result<f32, String> {
    opt_f32(v, key).ok_or_else(|| format!("missing numeric `{key}`"))
}

fn push_unique(vec: &mut Vec<String>, item: &str) {
    if !vec.iter().any(|p| p == item) {
        vec.push(item.to_string());
    }
}

/// `image: scene` keyword accessor for the screen layer.
pub fn is_scene(file: &str) -> bool {
    file == SCENE_KEYWORD
}
