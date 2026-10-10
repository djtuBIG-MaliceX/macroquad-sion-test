//! Letterboxed virtual canvas: all UI lives in a fixed 1280x720 space.
//! It renders into an offscreen target (where macroquad's `from_display_rect`
//! camera is orientation-correct) and the result is composited scaled into a
//! centered 16:9 rect of the real window, so resizing only grows/shrinks.

use std::cell::{Cell, RefCell};

use macroquad::prelude::*;

pub const BASE_WIDTH: f32 = 1280.0;
pub const BASE_HEIGHT: f32 = 720.0;

thread_local! {
    static TARGET: RefCell<Option<RenderTarget>> = const { RefCell::new(None) };
    /// (offset_x, offset_y, scale) of the composited rect for input mapping.
    static RECT: Cell<(f32, f32, f32)> = const { Cell::new((0.0, 0.0, 1.0)) };
    static IN_UI_PASS: Cell<bool> = const { Cell::new(false) };
    /// Current screen-shake displacement applied at composite time only;
    /// the letterbox stays black, so a shaken frame just rims with black.
    static SHAKE: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}

/// Displace the composited virtual canvas by a screen-shake offset (virtual
/// pixels). Runners set it every frame; `(0, 0)` restores a still image.
/// Input mapping deliberately ignores the shake.
pub fn set_shake(offset: Vec2) {
    SHAKE.set((offset.x, offset.y));
}

/// True while drawing inside the RT UI pass (`begin_frame`..`end_frame`).
pub fn in_ui_pass() -> bool {
    IN_UI_PASS.get()
}

/// Virtual-width helper to use everywhere instead of `screen_width`.
pub fn vw() -> f32 {
    BASE_WIDTH
}

/// Virtual-height helper to use everywhere instead of `screen_height`.
pub fn vh() -> f32 {
    BASE_HEIGHT
}

fn ensure_target() {
    TARGET.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(render_target(BASE_WIDTH as u32, BASE_HEIGHT as u32));
        }
    });
}

/// Call before drawing UI: switches into the fixed 1280x720 virtual space.
/// The host binary must have called `clear_background(BLACK)` first so the
/// letterbox bars are dark.
pub fn begin_frame() {
    ensure_target();
    IN_UI_PASS.set(true);
    set_pass_camera();
}

fn target_texture() -> Option<RenderTarget> {
    TARGET.with(|slot| slot.borrow().clone())
}

pub fn set_pass_camera() {
    let Some(target) = target_texture() else { return };

    // Camera bound to the render target: from_display_rect flips Y back
    // correctly here (render-target cameras use invert_y = +1).
    let camera = Camera2D {
        render_target: Some(target),
        ..Camera2D::from_display_rect(Rect {
            x: 0.0,
            y: 0.0,
            w: BASE_WIDTH,
            h: BASE_HEIGHT,
        })
    };

    set_camera(&camera);
}

/// Finish the frame UI pass: back to screen camera and stretch-composite the
/// virtual canvas into its centered 16:9 rect.
pub fn end_frame() {
    IN_UI_PASS.set(false);
    set_default_camera();

    TARGET.with(|slot| {
        let target = slot.borrow().clone().expect("canvas target");

        let (sw, sh) = (screen_width(), screen_height());
        let scale = (sw / BASE_WIDTH).min(sh / BASE_HEIGHT);
        let w = BASE_WIDTH * scale;
        let h = BASE_HEIGHT * scale;
        let x = (sw - w) * 0.5;
        let y = (sh - h) * 0.5;

        RECT.set((x, y, scale));

        // Screen shake displaces only the composite; `RECT` (input mapping)
        // stays at the resting position, and exposed bars are just black.
        let (shk_x, shk_y) = SHAKE.get();

        // GL framebuffer textures sample V-flipped relative to CPU-uploaded
        // textures (font atlas included); the composite corrects it once.
        draw_texture_ex(
            &target.texture,
            x + shk_x,
            y + shk_y,
            WHITE,
            DrawTextureParams { dest_size: Some(Vec2::new(w, h)), flip_y: true, ..Default::default() },
        );
    });
}

/// Pointer (mouse/touch) position mapped into the 1280x720 virtual space.
pub fn mouse_position() -> Vec2 {
    let (mx, my) = macroquad::input::mouse_position();
    let (x, y, s) = RECT.get();

    if s <= 0.0 {
        return Vec2::new(mx, my);
    }

    Vec2::new((mx - x) / s, (my - y) / s)
}

/// Render `rect`-sized procedural art into a fresh render target with a
/// top-left pixel camera matching its size.
///
/// GL framebuffer textures sample V-flipped relative to CPU-uploaded ones:
/// when drawing this texture inside a UI pass, pass `flip_y: true` in the
/// [`macroquad::DrawTextureParams`] so the chain resolves upright.
pub fn draw_to_target(w: u32, h: u32, f: impl FnOnce()) -> RenderTarget {
    let rt = render_target(w, h);

    let camera = Camera2D {
        render_target: Some(rt.clone()),
        ..Camera2D::from_display_rect(Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        })
    };

    set_camera(&camera);
    draw_rectangle(0.0, 0.0, w as f32, h as f32, BLACK);
    f();
    set_default_camera();

    rt
}
