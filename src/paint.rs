//! Ce qu'on pose sur le verre déjà calculé, à chaque image : rectangles arrondis, icônes, texte ; puis
//! l'affichage de la fenêtre en couches. Partagé par le lanceur et le panneau Paramètres.

use crate::render::{self, Rgb, Rgba, Rr};
use std::ptr::null_mut;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
};

/// Pixel prémultiplié `c` posé sur le pixel BGRA prémultiplié `d`.
pub fn over(d: u32, c: Rgba) -> u32 {
    let k = 1.0 - c[3];
    let ch = |sh: u32, v: f32| (((d >> sh & 255) as f32 * k + v * 255.0 + 0.5) as u32).min(255) << sh;
    ch(24, c[3]) | ch(16, c[0]) | ch(8, c[1]) | ch(0, c[2])
}

/// Rectangle arrondi plein (sélection, curseur, boutons), antialiasé.
pub fn fill(out: &mut [u32], w: usize, rr: Rr, color: Rgb, amount: f32) {
    let h = out.len() / w;
    let half = (rr.w * 0.5, rr.h * 0.5);
    for y in (rr.y.max(0.0) as usize)..((rr.y + rr.h).ceil() as usize).min(h) {
        for x in (rr.x.max(0.0) as usize)..((rr.x + rr.w).ceil() as usize).min(w) {
            let p = (x as f32 + 0.5 - rr.x - half.0, y as f32 + 0.5 - rr.y - half.1);
            let a = (0.5 - render::sd_rrect(p, half, rr.r)).clamp(0.0, 1.0) * amount;
            if a > 0.0 {
                out[y * w + x] = over(out[y * w + x], [color[0] * a, color[1] * a, color[2] * a, a]);
            }
        }
    }
}

/// Image `n`×`n` BGRA prémultipliée (icône) posée en (x0, y0).
pub fn blit(out: &mut [u32], w: usize, px: &[u32], n: usize, x0: usize, y0: usize) {
    let h = out.len() / w;
    for (j, row) in px.chunks(n).enumerate().take(h.saturating_sub(y0)) {
        for (i, &c) in row.iter().enumerate().take(w.saturating_sub(x0)) {
            if c >> 24 != 0 {
                let f = |sh: u32| (c >> sh & 255) as f32 / 255.0;
                let d = &mut out[(y0 + j) * w + x0 + i];
                *d = over(*d, [f(16), f(8), f(0), f(24)]);
            }
        }
    }
}

/// Opacités de texte (`win::text_alpha`, `lw` de large) posées en (x0, y0), à l'encre `ink`.
pub fn text(out: &mut [u32], w: usize, alpha: &[u8], lw: usize, x0: usize, y0: usize, ink: Rgb) {
    for (j, row) in alpha.chunks(lw).enumerate() {
        for (i, &a) in row.iter().enumerate() {
            if a > 0 {
                let a = a as f32 / 255.0;
                let d = &mut out[(y0 + j) * w + x0 + i];
                *d = over(*d, [ink[0] * a, ink[1] * a, ink[2] * a, a]);
            }
        }
    }
}

/// Affiche la bitmap sélectionnée dans `dc` à la place `win`, avec son alpha par pixel.
pub unsafe fn present(hwnd: HWND, dc: HDC, win: RECT) {
    GdiFlush();
    let pos = POINT { x: win.left, y: win.top };
    let size = SIZE { cx: win.right - win.left, cy: win.bottom - win.top };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
    UpdateLayeredWindow(hwnd, null_mut(), &pos, &size, dc, &src, 0, &blend, ULW_ALPHA);
}
