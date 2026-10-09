//! Internationalized UI/dialogue text. All user-facing strings are asset
//! files under `assets/i18n/<stem>.loc` (`key = value`, one per line);
//! English is the default and also the fallback when a key or an entire
//! locale asset is missing. The shipped tables are additionally embedded
//! with `include_str!` so the demo stays complete even if a fetch fails.

use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    English,
    Japanese,
}

impl Locale {
    pub const ALL: [Locale; 2] = [Locale::English, Locale::Japanese];

    /// Asset filename stem: `assets/i18n/<stem>.loc`.
    pub fn stem(self) -> &'static str {
        match self {
            Locale::English => "en",
            Locale::Japanese => "ja",
        }
    }

    /// Language picker label; always shown in its own script.
    pub fn native_name(self) -> &'static str {
        match self {
            Locale::English => "English",
            Locale::Japanese => "\u{65e5}\u{672c}\u{8a9e}",
        }
    }

    /// Wrapping ±1 step, used by the Options language row.
    pub fn shifted(self, dir: i32) -> Locale {
        let n = Self::ALL.len() as i32;
        let i = Self::ALL.iter().position(|l| *l == self).unwrap_or(0) as i32;
        Self::ALL[(((i + dir).rem_euclid(n)) as usize).min(Self::ALL.len() - 1)]
    }
}

pub struct Loc {
    active: Locale,
    tables: HashMap<Locale, HashMap<String, String>>,
}

fn parse(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key.trim().to_string(), value.trim_end().to_string());
        }
    }
    map
}

impl Loc {
    /// English-active table seeded from the embedded `.loc` assets.
    pub fn with_defaults() -> Loc {
        let mut tables = HashMap::new();
        for (locale, text) in [
            (Locale::English, include_str!("../assets/i18n/en.loc")),
            (Locale::Japanese, include_str!("../assets/i18n/ja.loc")),
        ] {
            tables.insert(locale, parse(text));
        }
        Loc { active: Locale::English, tables }
    }

    /// Overlay an externally-loaded `.loc` file onto a locale table.
    pub fn load(&mut self, locale: Locale, text: &str) {
        self.tables.entry(locale).or_default().extend(parse(text));
    }

    pub fn active(&self) -> Locale {
        self.active
    }

    pub fn set_active(&mut self, locale: Locale) {
        self.active = locale;
    }

    /// Resolve a key for the active locale, falling back to English then
    /// to the raw key so missing strings stay visible during development.
    pub fn t(&self, key: &str) -> Cow<'_, str> {
        if let Some(v) = self.tables.get(&self.active).and_then(|m| m.get(key)) {
            return Cow::Borrowed(v);
        }
        if self.active != Locale::English
            && let Some(v) = self.tables.get(&Locale::English).and_then(|m| m.get(key))
        {
            return Cow::Borrowed(v);
        }
        Cow::Owned(format!("?{key}"))
    }

    /// `t()` plus `{0}`-style positional substitution.
    pub fn fmt(&self, key: &str, args: &[&dyn std::fmt::Display]) -> String {
        let mut s = self.t(key).into_owned();
        for (i, arg) in args.iter().enumerate() {
            s = s.replace(&format!("{{{i}}}"), &arg.to_string());
        }
        s
    }
}
