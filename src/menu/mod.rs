//! The full screen stack of the demo: Title menu, a dialogue-scene stub
//! (procedural scene image + typewriter box from the scenario script), an
//! RPG-Maker-style save browser, the Options page (volumes + language) and
//! a fade-out Exit. Every interactive action routes into `crate::audio`,
//! which triggers precompiled SiON-rs material on the live music driver.
//!
//! All visible copy is asset-driven: UI strings from `assets/i18n/*.loc`
//! and scripted dialogue from `assets/scenario/*.scn` (l10n key refs), so
//! the Language option re-voices every screen at once. English is default.

use macroquad::prelude::*;

use crate::audio::{self, SfxId};
use crate::canvas;
use crate::l10n::Loc;
use crate::scenario::Scenario;
use crate::ui::theme;
use crate::ui::widgets::{FontSet, Menu, MenuEntry, Slider};

pub enum Outcome {
    Continue,
    Quit,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Title,
    Dialogue,
    LoadGame,
    Options,
    Exit,
}

struct DialogueState {
    page: usize,
    chars_shown: f32,
}

struct LoadState {
    selected: usize,
    tried_empty: bool,
}

/// Short crossfade between screens: fade to black over the first half,
/// swap modes at full black, fade back in over the second.
struct Transition {
    target: Mode,
    t: f32,
    switched: bool,
}

const FADE_HALF: f32 = 0.16;

const OPTIONS_ROWS: usize = 3; // music | sfx | language

struct OptionsState {
    row: usize,
    music: Slider,
    sfx: Slider,
}

const SLOT_COUNT: usize = 4;
const PORTRAIT_W: u32 = 480;
const PORTRAIT_H: u32 = 640;

pub struct App {
    pub fonts: FontSet,
    pub loc: Loc,
    mode: Mode,
    title_menu: Menu,

    portrait: RenderTarget,

    script: Scenario,
    dialogue: DialogueState,
    load_state: LoadState,
    options: OptionsState,

    sfx_gain: f32,
    exit_t: f32,
    transition: Option<Transition>,
}

impl App {
    pub fn new(fonts: FontSet, loc: Loc, script: Scenario) -> App {
        let portrait = gen_portrait_texture();

        let app = App {
            fonts,
            loc,
            mode: Mode::Title,
            title_menu: Menu::new(vec![
                MenuEntry { key: "menu.start" },
                MenuEntry { key: "menu.load" },
                MenuEntry { key: "menu.options" },
                MenuEntry { key: "menu.exit" },
            ]),
            portrait,
            script,
            dialogue: DialogueState { page: 0, chars_shown: 0.0 },
            load_state: LoadState { selected: 0, tried_empty: false },
            options: OptionsState {
                row: 0,
                music: Slider::new("options.music", 0.85),
                sfx: Slider::new("options.sfx", 0.75),
            },
            sfx_gain: 0.75,
            exit_t: 0.0,
            transition: None,
        };

        let music_v = app.options.music.value;
        audio::with(move |e| e.set_music_volume(music_v));
        app
    }

    /// Begin a short fade-to-black into `target`; input is swallowed until
    /// the crossfade completes. Exit keeps its own long farewell fade and is
    /// never a transition target.
    fn switch_to(&mut self, target: Mode) {
        if self.transition.is_none() && self.mode != target {
            self.transition = Some(Transition { target, t: 0.0, switched: false });
        }
    }

    /// Swap to `target` at the black midpoint and seed its fresh state there,
    /// so no screen pops in half-initialized.
    fn commit(&mut self, target: Mode) {
        match target {
            Mode::Dialogue => self.dialogue = DialogueState { page: 0, chars_shown: 0.0 },
            Mode::LoadGame => self.load_state = LoadState { selected: 0, tried_empty: false },
            Mode::Options => self.options.row = 0,
            _ => {}
        }
        self.mode = target;
    }

    fn play_sfx(&mut self, id: SfxId) {
        let gain = self.sfx_gain;
        audio::with(move |e| e.play_sfx(id, gain));
    }

    /// Language row value label; always in the target language's own script.
    fn lang_name(&self) -> &'static str {
        self.loc.active().native_name()
    }

    // -------------------------------------------------------------- input/pump

    pub fn update(&mut self, dt: f32) -> Outcome {
        // In-flight crossfade: advance it and swallow all input until done.
        if let Some(mut tr) = self.transition.take() {
            tr.t += dt;
            if !tr.switched && tr.t >= FADE_HALF {
                self.commit(tr.target);
                tr.switched = true;
            }
            if tr.t < FADE_HALF * 2.0 {
                self.transition = Some(tr);
            }
            self.title_menu.tick(dt);
            return Outcome::Continue;
        }

        let pressed = |k| is_key_pressed(k);

        match self.mode {
            Mode::Title => {
                if (pressed(KeyCode::Up) || pressed(KeyCode::W)) && self.title_menu.move_up() {
                    self.play_sfx(SfxId::Move);
                }
                if (pressed(KeyCode::Down) || pressed(KeyCode::S)) && self.title_menu.move_down() {
                    self.play_sfx(SfxId::Move);
                }

                if pressed(KeyCode::Enter) || pressed(KeyCode::Space) || pressed(KeyCode::Z) {
                    match self.title_menu.selected() {
                        0 => {
                            self.play_sfx(SfxId::Confirm);
                            self.switch_to(Mode::Dialogue);
                        }
                        1 => {
                            self.play_sfx(SfxId::Confirm);
                            self.switch_to(Mode::LoadGame);
                        }
                        2 => {
                            self.play_sfx(SfxId::Confirm);
                            self.switch_to(Mode::Options);
                        }
                        _ => {
                            // Exit: fade audio once, visuals follow via exit_t.
                            self.play_sfx(SfxId::Exit);
                            audio::with(|e| {
                                e.stop_music_looping();
                                e.fade_out_master(1.2);
                            });
                            self.exit_t = 0.0;
                            self.mode = Mode::Exit;
                        }
                    }
                }

                if pressed(KeyCode::Escape) || pressed(KeyCode::X) {
                    self.play_sfx(SfxId::Exit);
                    audio::with(|e| {
                        e.stop_music_looping();
                        e.fade_out_master(1.2);
                    });
                    self.exit_t = 0.0;
                    self.mode = Mode::Exit;
                }
            }

            Mode::Dialogue => {
                self.dialogue.chars_shown += dt * 34.0;

                if pressed(KeyCode::Enter) || pressed(KeyCode::Space) || pressed(KeyCode::Z) {
                    let page = &self.script.pages[self.dialogue.page];
                    let total = self.loc.t(&page.text_key).chars().count();
                    if (self.dialogue.chars_shown as usize) < total {
                        self.dialogue.chars_shown = total as f32; // reveal all
                    } else if self.dialogue.page + 1 < self.script.pages.len() {
                        self.play_sfx(SfxId::Move);
                        self.dialogue.page += 1;
                        self.dialogue.chars_shown = 0.0;
                    } else {
                        self.play_sfx(SfxId::Back);
                        self.switch_to(Mode::Title);
                    }
                } else if pressed(KeyCode::Escape) || pressed(KeyCode::X) {
                    self.play_sfx(SfxId::Back);
                    self.switch_to(Mode::Title);
                }
            }

            Mode::LoadGame => {
                if (pressed(KeyCode::Up) || pressed(KeyCode::W)) && self.load_state.selected > 0 {
                    self.load_state.selected -= 1;
                    self.play_sfx(SfxId::Move);
                }
                if (pressed(KeyCode::Down) || pressed(KeyCode::S))
                    && self.load_state.selected + 1 < SLOT_COUNT
                {
                    self.load_state.selected += 1;
                    self.play_sfx(SfxId::Move);
                }

                if pressed(KeyCode::Enter) || pressed(KeyCode::Space) || pressed(KeyCode::Z) {
                    self.play_sfx(SfxId::Confirm);
                    self.load_state.tried_empty = true;
                } else if pressed(KeyCode::Escape) || pressed(KeyCode::X) {
                    self.play_sfx(SfxId::Back);
                    self.switch_to(Mode::Title);
                }
            }

            Mode::Options => {
                if (pressed(KeyCode::Up) || pressed(KeyCode::W)) && self.options.row > 0 {
                    self.options.row -= 1;
                    self.play_sfx(SfxId::Move);
                } else if (pressed(KeyCode::Down) || pressed(KeyCode::S))
                    && self.options.row + 1 < OPTIONS_ROWS
                {
                    self.options.row += 1;
                    self.play_sfx(SfxId::Move);
                }

                let mut dir = 0i32;
                if pressed(KeyCode::Left) || pressed(KeyCode::A) {
                    dir = -1;
                } else if pressed(KeyCode::Right) || pressed(KeyCode::D) {
                    dir = 1;
                }
                if dir != 0 {
                    match self.options.row {
                        0 => {
                            if self.options.music.adjust(dir) {
                                let v = self.options.music.value;
                                audio::with(move |e| e.set_music_volume(v));
                            }
                        }
                        1 => {
                            if self.options.sfx.adjust(dir) {
                                self.sfx_gain = self.options.sfx.value;
                            }
                            // Programmatic SiON path: pitched note_on blip.
                            let v = self.options.sfx.value;
                            audio::with(move |e| e.slider_blip(v, v.max(0.15)));
                        }
                        _ => {
                            // Language row: live-switch every string in place.
                            let next = self.loc.active().shifted(dir);
                            self.loc.set_active(next);
                            self.play_sfx(SfxId::Move);
                        }
                    }
                }

                if pressed(KeyCode::Escape) || pressed(KeyCode::X) {
                    self.play_sfx(SfxId::Back);
                    self.switch_to(Mode::Title);
                }
            }

            Mode::Exit => {
                self.exit_t += dt;
                if self.exit_t >= 1.5 {
                    return Outcome::Quit;
                }
            }
        }

        self.title_menu.tick(dt);
        Outcome::Continue
    }

    // ------------------------------------------------------------------- draw

    pub fn draw(&self) {
        theme::draw_gradient_background();

        match self.mode {
            Mode::Title => self.draw_title(),
            Mode::Dialogue => self.draw_dialogue(),
            Mode::LoadGame => self.draw_loadgame(),
            Mode::Options => self.draw_options(),
            Mode::Exit => {}
        }

        match self.mode {
            Mode::Title | Mode::LoadGame => self.draw_footer(),
            _ => {}
        }

        if let Some(tr) = &self.transition {
            // triangle wave through full black at the midpoint, smoothstepped
            let p = if tr.t < FADE_HALF { tr.t / FADE_HALF } else { (2.0 * FADE_HALF - tr.t) / FADE_HALF };
            let a = p.clamp(0.0, 1.0);
            theme::dim(a * a * (3.0 - 2.0 * a));
        }

        if self.mode == Mode::Exit {
            let t = (self.exit_t / 1.3).clamp(0.0, 1.0);
            theme::dim(t * t * (3.0 - 2.0 * t)); // smoothstep fade-to-black
            let a = ((t - 0.35) * 2.4).clamp(0.0, 1.0);
            if a > 0.0 {
                self.fonts.text_center(
                    &self.loc.t("exit.farewell"),
                    canvas::vw() * 0.5,
                    canvas::vh() * 0.52,
                    40.0,
                    Color::new(1.0, 0.92, 0.78, a),
                );
            }
        }
    }

    fn draw_title(&self) {
        let cx = canvas::vw() * 0.5;
        self.fonts.text_center(&self.loc.t("game.title"), cx, 170.0, 76.0, theme::GOLD);

        let menu_x = canvas::vw() * 0.62;
        self.title_menu.draw(&self.fonts, &self.loc, menu_x, canvas::vh() * 0.34);

        // standing key-art stub at the left
        let h = canvas::vh() * 0.72;
        let w = h * (PORTRAIT_W as f32 / PORTRAIT_H as f32);
        draw_texture_ex(
            &self.portrait.texture,
            canvas::vw() * 0.2 - w * 0.5,
            canvas::vh() - h - 44.0,
            WHITE,
            DrawTextureParams { dest_size: Some(Vec2::new(w, h)), flip_y: true, ..Default::default() },
        );
    }

    fn draw_dialogue(&self) {
        // full-bleed sample scene standing in for the real background art
        let sh = canvas::vh();
        let sw = canvas::vw();
        draw_texture_ex(
            &self.portrait.texture,
            0.0,
            -24.0,
            WHITE,
            DrawTextureParams { dest_size: Some(Vec2::new(sh * 0.75, sh + 48.0)), flip_y: true, ..Default::default() },
        );
        // darken sides so the panel reads over the scene
        let fade = sh * 0.18;
        draw_rectangle(0.0, 0.0, fade, sh, Color::new(0.02, 0.02, 0.05, 0.4));
        draw_rectangle(sw - fade, 0.0, fade, sh, Color::new(0.02, 0.02, 0.05, 0.4));

        let bw = sw * 0.76;
        let bh = 192.0;
        let bx = (sw - bw) * 0.5;
        let by = sh - bh - 34.0;
        theme::draw_panel(bx, by, bw, bh);

        let page = &self.script.pages[self.dialogue.page];
        let speaker = self.loc.t(&page.speaker_key);
        let line = self.loc.t(&page.text_key);

        self.fonts.text(&speaker, bx + 30.0, by - 16.0, 32.0, theme::CURSOR);
        draw_rectangle(
            bx + 26.0,
            by - 6.0,
            self.fonts.measure(&speaker, 32.0) + 18.0,
            4.0,
            theme::CURSOR,
        );

        // typewriter over the localized text (switch languages freely)
        let total = line.chars().count();
        let shown: String = line.chars().take((self.dialogue.chars_shown as usize).min(total)).collect();
        self.fonts.text(&shown, bx + 34.0, by + 78.0, 30.0, theme::INK);

        if (self.dialogue.chars_shown as usize) >= total && (get_time() * 3.0).sin() > 0.0 {
            let sx = bx + bw - 46.0;
            draw_triangle(
                Vec2::new(sx, by + bh - 48.0),
                Vec2::new(sx + 20.0, by + bh - 48.0),
                Vec2::new(sx + 10.0, by + bh - 32.0),
                theme::GOLD,
            );
        }

        self.fonts.text(
            &self.loc.t("dialogue.hint"),
            bx + 4.0,
            by - 16.0,
            20.0,
            theme::INK_DIM,
        );
    }

    fn draw_loadgame(&self) {
        let (sw, sh) = (canvas::vw(), canvas::vh());

        self.fonts.text_center(&self.loc.t("load.title"), sw * 0.5, 96.0, 48.0, theme::GOLD);
        self.fonts.text_center(
            &self.loc.t("load.subtitle"),
            sw * 0.5,
            148.0,
            23.0,
            theme::INK_DIM,
        );

        // RPG-Maker style: slot grid left, detail plate right column bottom
        let slot_w = 430.0;
        let slot_h = 128.0;
        let gap = 16.0;
        let x0 = sw * 0.5 - slot_w - gap / 2.0;
        let y0 = sh * 0.26;

        for i in 0..SLOT_COUNT {
            let sel = self.load_state.selected == i;
            let x = x0 + if i % 2 == 0 { 0.0 } else { slot_w + gap };
            let y = y0 + (i / 2) as f32 * (slot_h + gap);

            draw_rectangle(
                x, y, slot_w, slot_h,
                if sel { Color::new(0.145, 0.165, 0.23, 0.96) } else { theme::PANEL },
            );
            theme::border(
                x, y, slot_w, slot_h,
                if sel { 3.0 } else { 1.5 },
                if sel { theme::CURSOR } else { theme::PANEL_EDGE_DIM },
            );

            draw_texture_ex(
                &self.portrait.texture,
                x + 10.0, y + 8.0,
                if sel { WHITE } else { Color::new(0.62, 0.64, 0.71, 1.0) },
                DrawTextureParams { dest_size: Some(Vec2::new(slot_h * 0.48, slot_h - 16.0)), flip_y: true, ..Default::default() },
            );

            let tx = x + slot_h * 0.48 + 30.0;
            self.fonts.text(
                &self.loc.fmt("load.slot", &[&(i + 1)]),
                tx, y + 42.0, 27.0,
                if sel { theme::GOLD } else { theme::INK },
            );
            self.fonts.text(&self.loc.t("load.stats"), tx, y + 76.0, 21.0, theme::INK_DIM);
            self.fonts.text(&self.loc.t("load.empty"), tx, y + 104.0, 20.0, theme::INK_DIM);
        }

        // detail plate under the grid (classic RPG window box)
        let dx = x0;
        let dy = y0 + 2.0 * slot_h + gap + 14.0;
        theme::draw_panel(dx, dy, slot_w * 2.0 + gap, sh - dy - 110.0);
        self.fonts.text(
            &self.loc.t("load.plate"),
            dx + 30.0, dy + 46.0, 24.0, theme::INK_DIM,
        );

        if self.load_state.tried_empty {
            self.fonts.text_center(
                &self.loc.t("load.none"),
                sw * 0.5, dy + 92.0, 26.0, theme::GOLD,
            );
        }

        self.fonts.text_center(
            &self.loc.t("load.hint"),
            sw * 0.5, sh - 62.0, 21.0, theme::INK_DIM,
        );
    }

    fn draw_options(&self) {
        let (sw, sh) = (canvas::vw(), canvas::vh());

        self.fonts.text_center(&self.loc.t("options.title"), sw * 0.5, 96.0, 48.0, theme::GOLD);

        let panel_w = 820.0;
        let px = (sw - panel_w) * 0.5;
        let py = sh * 0.28 - 16.0;
        theme::draw_panel(px, py, panel_w, 384.0);

        Slider::draw_hint(&self.fonts, &self.loc, px + 34.0, py - 16.0);

        let bar_w = panel_w - 380.0;
        self.options.music.draw(&self.fonts, &self.loc, px + 74.0, py + 60.0, bar_w, self.options.row == 0);
        self.options.sfx.draw(&self.fonts, &self.loc, px + 74.0, py + 60.0 + 128.0, bar_w, self.options.row == 1);
        Slider::draw_choice(
            &self.fonts,
            &self.loc,
            "options.language",
            self.lang_name(),
            px + 74.0,
            py + 60.0 + 296.0,
            self.options.row == 2,
        );

        self.fonts.text(
            &self.loc.t("options.note"),
            px + 34.0, sh - 128.0, 19.0, theme::INK_DIM,
        );

        // language selector footer: current pick stays visible after backing out
        self.fonts.text_center(
            &format!("[ {} ]", self.lang_name()),
            sw - 96.0, sh - 34.0, 19.0, theme::INK_DIM,
        );
    }

    fn draw_footer(&self) {
        let (sw, sh) = (canvas::vw(), canvas::vh());
        self.fonts.text(
            &self.loc.fmt("footer.engine", &[&libsion::VERSION]),
            28.0,
            sh - 34.0,
            19.0,
            theme::INK_DIM,
        );

        if self.mode == Mode::Title {
            self.fonts.text_center(
                &self.loc.t("title.hint"),
                sw * 0.5, sh - 62.0, 21.0, theme::INK_DIM,
            );
        }
    }
}

/// Placeholder key-art texture: dusk sky, sun over hills and a lone
/// traveler. Replace with real PNGs in `assets/images/` later.
fn gen_portrait_texture() -> RenderTarget {
    let (w, h) = (PORTRAIT_W as f32, PORTRAIT_H as f32);

    canvas::draw_to_target(PORTRAIT_W, PORTRAIT_H, || {
        let bands = 32.0;
        for i in 0..bands as u32 {
            let t = i as f32 / (bands - 1.0);
            draw_rectangle(
                0.0,
                h * t,
                w,
                h / bands + 2.0,
                mix(
                    Color::new(0.13, 0.12, 0.26, 1.0),
                    Color::new(0.45, 0.32, 0.25, 1.0),
                    t,
                ),
            );
        }

        for i in 0..6u32 {
            draw_circle(
                w * 0.35,
                h * 0.66 + i as f32 * 4.0,
                52.0 + i as f32 * 12.0,
                Color::new(1.0, 0.75, 0.42, 0.1),
            );
        }
        draw_circle(w * 0.35, h * 0.68, 62.0, Color::new(1.0, 0.84, 0.55, 0.95));

        draw_circle(w * 0.25, h * 1.24, 360.0, Color::new(0.17, 0.21, 0.31, 1.0));
        draw_circle(w * 0.92, h * 1.34, 420.0, Color::new(0.12, 0.15, 0.24, 1.0));
        draw_rectangle(0.0, h * 0.86, w, h * 0.2, Color::new(0.09, 0.11, 0.18, 1.0));

        // traveler
        draw_circle(w * 0.68, h * 0.735, 21.0, Color::new(0.04, 0.05, 0.09, 1.0));
        draw_triangle(
            Vec2::new(w * 0.68, h * 0.735),
            Vec2::new(w * 0.68 - 27.0, h * 0.97),
            Vec2::new(w * 0.68 + 27.0, h * 0.97),
            Color::new(0.04, 0.05, 0.09, 1.0),
        );
        draw_line(
            w * 0.75,
            h * 0.74,
            w * 0.82,
            h * 0.97,
            5.0,
            Color::new(0.32, 0.24, 0.16, 1.0),
        );

        for (bx, by) in [
            (w * 0.26, h * 0.25),
            (w * 0.38, h * 0.21),
            (w * 0.52, h * 0.27),
        ] {
            draw_line(bx - 10.0, by, bx, by + 7.0, 2.5, Color::new(0.1, 0.1, 0.15, 1.0));
            draw_line(bx + 10.0, by, bx, by + 7.0, 2.5, Color::new(0.1, 0.1, 0.15, 1.0));
        }
    })
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        1.0,
    )
}
