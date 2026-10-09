//! Scenario scripts: the in-game text is data, not code. A `.scn` script
//! is a sequence of `[page]` blocks whose `speaker`/`text` entries are
//! l10n keys, resolved against the active locale at presentation time.
//! Future scripting (conditions, choices, staging commands) extends this
//! parser without touching the screens.

/// One dialogue beat; both fields are keys into [`crate::l10n::Loc`].
pub struct Page {
    pub speaker_key: String,
    pub text_key: String,
}

pub struct Scenario {
    pub pages: Vec<Page>,
}

impl Scenario {
    /// Parse the `.scn` format: blank lines and `#` comments ignored, a
    /// `[page]` header opens a block, `key = value` fills its fields.
    pub fn parse(text: &str) -> Scenario {
        let mut pages = Vec::new();
        let mut speaker = String::new();
        let mut body = String::new();
        let mut in_page = false;

        for raw in text.lines() {
            let line = raw.trim_start_matches('\u{feff}').trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[page]" {
                if in_page && !body.is_empty() {
                    pages.push(Page { speaker_key: speaker, text_key: body });
                }
                speaker = String::new();
                body = String::new();
                in_page = true;
            } else if let Some((k, v)) = line.split_once('=') {
                match k.trim() {
                    "speaker" => speaker = v.trim().to_string(),
                    "text" => body = v.trim().to_string(),
                    _ => {} // unknown directives: forward-compatible no-ops
                }
            }
        }
        if in_page && !body.is_empty() {
            pages.push(Page { speaker_key: speaker, text_key: body });
        }

        Scenario { pages }
    }

    /// The shipped prologue, embedded so the demo runs without asset I/O.
    pub fn prologue() -> Scenario {
        Self::parse(include_str!("../assets/scenario/prologue.scn"))
    }
}
