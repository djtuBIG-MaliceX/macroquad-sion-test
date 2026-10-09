//! Palette + shared draw helpers: the fade-gradient background stub,
//! window chrome and panel plates.

use macroquad::prelude::*;

use crate::canvas;

pub const BG_TOP: Color = Color::new(0.035, 0.047, 0.098, 1.0);
pub const BG_BOTTOM: Color = Color::new(0.129, 0.078, 0.051, 1.0);
pub const GLOW: Color = Color::new(0.36, 0.24, 0.13, 0.5);

pub const PANEL: Color = Color::new(0.086, 0.098, 0.137, 0.92);
pub const PANEL_EDGE: Color = Color::new(0.45, 0.37, 0.21, 1.0);
pub const PANEL_EDGE_DIM: Color = Color::new(0.26, 0.22, 0.15, 1.0);

pub const INK: Color = Color::new(0.87, 0.89, 0.93, 1.0);
pub const INK_DIM: Color = Color::new(0.52, 0.55, 0.62, 1.0);
pub const GOLD: Color = Color::new(1.0, 0.82, 0.35, 1.0);
pub const GOLD_DIM: Color = Color::new(0.62, 0.51, 0.24, 1.0);
pub const CURSOR: Color = Color::new(1.0, 0.68, 0.2, 1.0);

/// Vertical two-stop gradient background (stub stands in for real art).
/// The host binary clears the window black first, so only this base rect
/// is painted and the letterbox bars stay dark.
pub fn draw_gradient_background() {
    let screen_w = canvas::vw();
    let screen_h = canvas::vh();

    // bottom fade: stacked translucent bands blended toward BG_BOTTOM
    let bands = 48u32;
    for i in 0..bands {
        let t = i as f32 / (bands - 1) as f32;
        let color = Color {
            r: BG_TOP.r + (BG_BOTTOM.r - BG_TOP.r) * t,
            g: BG_TOP.g + (BG_BOTTOM.g - BG_TOP.g) * t,
            b: BG_TOP.b + (BG_BOTTOM.b - BG_TOP.b) * t,
            a: 1.0,
        };
        let y = screen_h * t;
        draw_rectangle(0.0, y - 1.0, screen_w, screen_h / bands as f32 + 2.0, color);
    }

    // soft "horizon" glow so the stub has a focal point behind the menu
    let cx = screen_w * 0.5;
    let cy = screen_h * 1.08;
    for i in 0..26u32 {
        let t = i as f32 / 25.0;
        draw_circle(cx, cy, screen_w * (0.18 + 0.32 * t), Color::new(GLOW.r, GLOW.g, GLOW.b, (1.0 - t) * 0.045));
    }

    draw_vignette();
}

fn draw_vignette() {
    let (w, h) = (canvas::vw(), canvas::vh());
    let edge = 140.0f32;
    for i in 0..6u32 {
        let a = (0.10 - i as f32 * 0.015).max(0.0);
        let c = Color::new(0.0, 0.0, 0.02, a);
        draw_rectangle(0.0, 0.0, w, i as f32 * (edge / 6.0) + edge / 6.0, c);
        draw_rectangle(0.0, h - (i as f32 * (edge / 6.0) + edge / 6.0), w, edge, c);
    }
}

/// Axis-aligned frame of `t`-thick edges. Unlike `draw_rectangle_lines`
/// (unmitred thick lines blob or gap at corners) this keeps 90 deg joins
/// pixel-crisp.
pub fn border(x: f32, y: f32, w: f32, h: f32, t: f32, color: Color) {
    draw_rectangle(x, y, w, t, color); // top
    draw_rectangle(x, y + h - t, w, t, color); // bottom
    draw_rectangle(x, y, t, h, color); // left
    draw_rectangle(x + w - t, y, t, h, color); // right
}

/// RPG-style double-frame panel.
pub fn draw_panel(x: f32, y: f32, w: f32, h: f32) {
    let (ix, iy, iw, ih) = (x - 3.0, y - 3.0, w + 6.0, h + 6.0);
    let (x2, y2) = (ix + iw, iy + ih);

    draw_rectangle(x - 6.0, y - 6.0, w + 12.0, h + 12.0, PANEL_EDGE_DIM);
    draw_rectangle(ix, iy, iw, ih, PANEL);
    border(ix, iy, iw, ih, 2.0, PANEL_EDGE);

    // corner accents: paired arms flush with the frame edges
    let c = CURSOR;
    let (a, t) = (16.0f32, 3.0f32);
    draw_rectangle(ix, iy, a, t, c); // top-left horizontal
    draw_rectangle(ix, iy, t, a, c); // top-left vertical
    draw_rectangle(x2 - a, iy, a, t, c); // top-right horizontal
    draw_rectangle(x2 - t, iy, t, a, c); // top-right vertical
    draw_rectangle(ix, y2 - t, a, t, c); // bottom-left horizontal
    draw_rectangle(ix, y2 - a, t, a, c); // bottom-left vertical
    draw_rectangle(x2 - a, y2 - t, a, t, c); // bottom-right horizontal
    draw_rectangle(x2 - t, y2 - a, t, a, c); // bottom-right vertical
}

pub fn dim(alpha: f32) {
    draw_rectangle(0.0, 0.0, canvas::vw(), canvas::vh(), Color::new(0.0, 0.0, 0.02, alpha));
}
