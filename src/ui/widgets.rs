//! Interactive widgets: vertical menu list and value sliders. Pure state +
//! draw; audio triggers are returned to the screen layer (which fires
//! precompiled bank SFX), keeping widgets reusable outside this demo.

use macroquad::prelude::*;

use crate::ui::theme;

pub struct FontSet {
    pub regular: Option<Font>,
}

impl FontSet {
    /// Build from raw TTF bytes (already loaded async by the caller);
    /// fall back to macroquad's built-in default when parsing fails.
    pub fn from_bytes(bytes: Option<&[u8]>, label: &str) -> Self {
        let Some(bytes) = bytes else {
            eprintln!("no font bytes for {label} - using built-in default");
            return FontSet { regular: None };
        };

        // fontdue may panic on exotic face tables - keep the demo alive.
        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            macroquad::text::load_ttf_font_from_bytes(bytes)
        }));

        match loaded {
            Ok(Ok(font)) => {
                eprintln!("font loaded: {label}");
                FontSet { regular: Some(font) }
            }
            Ok(Err(e)) => {
                eprintln!("font load failed for {label} ({e:?}) - built-in default");
                FontSet { regular: None }
            }
            Err(_) => {
                eprintln!("font parser panicked on {label} - built-in default");
                FontSet { regular: None }
            }
        }
    }

    pub fn text(&self, s: &str, x: f32, y: f32, size: f32, color: Color) {
        // NOTE: inside the canvas RT pass the standard camera already yields
        // correct orientation for text and geometry; do NOT re-mirror here.
        draw_text_ex(
            s,
            x,
            y,
            TextParams {
                font: self.regular.as_ref(),
                font_size: size.max(1.0).round() as u16,
                color,
                ..Default::default()
            },
        );
    }

    pub fn measure(&self, s: &str, size: f32) -> f32 {
        measure_text(
            s,
            self.regular.as_ref(),
            size.max(1.0).round() as u16,
            1.0,
        )
        .width
    }

    /// Centered horizontally on the given screen x.
    pub fn text_center(&self, s: &str, cx: f32, y: f32, size: f32, color: Color) {
        let x = cx - self.measure(s, size) * 0.5;
        self.text(s, x, y, size, color);
    }
}

pub struct MenuEntry {
    /// l10n key resolved through the active locale at draw time.
    pub key: &'static str,
}

pub struct Menu {
    entries: Vec<MenuEntry>,
    selected: usize,
    bob: f32,
}

impl Menu {
    pub fn new(entries: Vec<MenuEntry>) -> Self {
        Menu { entries, selected: 0, bob: 0.0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn set_selected(&mut self, i: usize) {
        self.selected = i.min(self.entries.len().saturating_sub(1));
    }

    /// Returns true when the selection actually moved (caller fires SFX).
    pub fn move_up(&mut self) -> bool {
        if self.selected == 0 {
            return false;
        }
        self.selected -= 1;
        true
    }

    pub fn move_down(&mut self) -> bool {
        if self.selected + 1 >= self.entries.len() {
            return false;
        }
        self.selected += 1;
        true
    }

    /// Drives the cursor bob animation.
    pub fn tick(&mut self, dt: f32) {
        self.bob += dt;
    }

    /// One label per row, resolved via `loc` — a row shows only the active
    /// language's text (English by default; see Options > Language).
    pub fn draw(&self, fonts: &FontSet, loc: &crate::l10n::Loc, x: f32, y: f32) {
        let row_h = 84.0;
        for (i, entry) in self.entries.iter().enumerate() {
            let is_cursor = i == self.selected;
            let ry = y + i as f32 * row_h;
            let bob = self.bob.sin() * 2.0;
            let label = loc.t(entry.key);

            if is_cursor {
                let w = fonts.measure(&label, 40.0);
                theme::draw_panel(x - 46.0 + bob, ry - 34.0, w + 92.0, row_h);
            }

            // single line vertically centered in the row (baseline below cap)
            let baseline = ry + 16.0;
            fonts.text(
                &label,
                x + bob,
                baseline,
                40.0,
                if is_cursor { theme::GOLD } else { theme::INK_DIM },
            );

            if is_cursor {
                // arrow pointing at the label, inside the left gutter
                let back = x - 36.0 + bob; // vertical base edge (x)
                let tip = back + 15.0;
                let cy = baseline - 13.0;
                draw_triangle(
                    Vec2::new(back, cy - 10.0),
                    Vec2::new(back, cy + 10.0),
                    Vec2::new(tip, cy),
                    theme::CURSOR,
                );
            }
        }
    }

    pub fn height(&self) -> f32 {
        self.entries.len() as f32 * 84.0
    }
}

pub struct Slider {
    /// l10n key for the row label.
    pub key: &'static str,
    pub value: f32, // normalized 0..1; display mapping done by owner
}

impl Slider {
    pub fn new(key: &'static str, value: f32) -> Self {
        Slider { key, value: value.clamp(0.0, 1.0) }
    }

    /// Steps the value; returns true when it actually changed.
    pub fn adjust(&mut self, dir: i32) -> bool {
        let old = self.value;
        self.value = (self.value + dir as f32 * 0.05).clamp(0.0, 1.0);
        (self.value - old).abs() > f32::EPSILON
    }

    pub fn draw(&self, fonts: &FontSet, loc: &crate::l10n::Loc, x: f32, y: f32, w: f32, active: bool) {
        fonts.text(&loc.t(self.key), x, y, 28.0, if active { theme::GOLD } else { theme::INK });

        let bar_y = y + 40.0;
        let track = if active { theme::PANEL_EDGE_DIM } else { Color::new(0.12, 0.13, 0.17, 1.0) };
        draw_rectangle(x, bar_y - 7.0, w, 14.0, track);
        theme::border(x - 2.0, bar_y - 9.0, w + 4.0, 18.0, 2.0, theme::PANEL_EDGE);

        let fill_w = (w - 8.0) * self.value;
        draw_rectangle(
            x + 4.0,
            bar_y - 5.0,
            fill_w.max(2.0),
            10.0,
            if active { theme::GOLD } else { theme::GOLD_DIM },
        );

        let knob_x = x + 4.0 + fill_w;
        theme::border(
            knob_x - 6.0,
            bar_y - 12.0,
            12.0,
            24.0,
            3.0,
            if active { theme::CURSOR } else { theme::INK_DIM },
        );

        let pct = format!("{:.0}%", self.value * 100.0);
        let pw = fonts.measure(&pct, 22.0);
        fonts.text(
            &pct,
            x + w - pw + 34.0,
            bar_y + 8.0,
            22.0,
            if active { theme::INK } else { theme::INK_DIM },
        );

        if active {
            fonts.text("<", x - 34.0, bar_y + 8.0, 26.0, theme::CURSOR);
            fonts.text(">", x + w + 14.0, bar_y + 8.0, 26.0, theme::CURSOR);
        }
    }

    /// Language-picker row: label plus boxed `< native-name >` value.
    pub fn draw_choice(
        fonts: &FontSet,
        loc: &crate::l10n::Loc,
        key: &'static str,
        value: &str,
        x: f32,
        y: f32,
        active: bool,
    ) {
        let label = loc.t(key);
        fonts.text(&label, x, y, 28.0, if active { theme::GOLD } else { theme::INK });

        let vx = x + 380.0;
        if active {
            fonts.text("<", vx - 44.0, y, 30.0, theme::CURSOR);
            fonts.text(">", vx + 44.0 + fonts.measure(value, 30.0), y, 30.0, theme::CURSOR);
        }
        let vw = fonts.measure(value, 30.0);
        draw_rectangle(
            vx - 14.0,
            y - 32.0,
            vw + 28.0,
            46.0,
            if active { Color::new(0.145, 0.165, 0.23, 0.96) } else { theme::PANEL },
        );
        theme::border(
            vx - 14.0,
            y - 32.0,
            vw + 28.0,
            46.0,
            if active { 3.0 } else { 1.5 },
            if active { theme::CURSOR } else { theme::PANEL_EDGE_DIM },
        );
        fonts.text(
            value,
            vx,
            y,
            30.0,
            if active { theme::GOLD } else { theme::INK_DIM },
        );
    }

    pub fn draw_hint(fonts: &FontSet, loc: &crate::l10n::Loc, x: f32, y: f32) {
        fonts.text(&loc.t("options.hint"), x, y, 20.0, theme::INK_DIM);
    }
}
