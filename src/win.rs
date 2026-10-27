//! Ce qui parle à Windows hors rendu : texte, capture du fond, icône de notification, menu.
//! `w`, `dib`, `text_layer`, `capture`, `tray`, `notify`, `menu` viennent de Deskorb (eux-mêmes de DESKTOUCH).

use crate::render::{Back, Rgba};
use std::{
    ffi::c_void,
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Shell::*, WindowsAndMessaging::*},
};

pub fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// DIB 32 bits haut-en-bas : (bitmap, pointeur vers les pixels).
pub unsafe fn dib(w: i32, h: i32) -> (HBITMAP, *mut u32) {
    let mut bmi: BITMAPINFO = zeroed();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h,
        biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..zeroed()
    };
    let mut bits: *mut c_void = null_mut();
    let bmp = CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    (bmp, bits as *mut u32)
}

fn unpack(p: u32) -> Rgba {
    [(p >> 16 & 255) as f32 / 255.0, (p >> 8 & 255) as f32 / 255.0, (p & 255) as f32 / 255.0, (p >> 24) as f32 / 255.0]
}

/// Texte à poser sur un calque.
pub struct Text<'a> {
    pub s: &'a str,
    pub r: RECT,
    pub size: f32,
    pub weight: i32,
    pub flags: DRAW_TEXT_FORMAT,
    pub level: f32, // 1 = plein, < 1 = texte secondaire
}

/// Police d'un texte : les caractères de la zone privée Unicode sont les pictogrammes de Windows 11 (loupe = E721).
unsafe fn font(s: &str, size: f32, weight: i32) -> HFONT {
    let glyphs = !s.is_empty() && s.chars().all(|c| ('\u{E000}'..='\u{F8FF}').contains(&c));
    let face = if glyphs { "Segoe Fluent Icons" } else { "Segoe UI Variable Text" };
    CreateFontW(-size.round() as i32, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0,
        ANTIALIASED_QUALITY as u32, 0, w(face).as_ptr())
}

/// Opacité des textes, un octet par pixel (texte blanc) : GDI dessine en blanc sur noir, la luminosité
/// devient l'opacité. 16 fois moins de mémoire à remplir et relire qu'un `Layer`, à chaque frappe.
pub unsafe fn text_alpha(pw: usize, ph: usize, texts: &[Text]) -> Vec<u8> {
    let (bmp, bits) = dib(pw as i32, ph as i32);
    let dc = CreateCompatibleDC(null_mut());
    let old = SelectObject(dc, bmp);
    SetBkMode(dc, TRANSPARENT as i32);
    for t in texts {
        let f = font(t.s, t.size, t.weight);
        let of = SelectObject(dc, f);
        let g = (255.0 * t.level).round() as u32;
        SetTextColor(dc, g | g << 8 | g << 16);
        let txt: Vec<u16> = t.s.encode_utf16().collect();
        let mut r = t.r;
        DrawTextW(dc, txt.as_ptr(), txt.len() as i32, &mut r, t.flags | DT_NOPREFIX | DT_SINGLELINE);
        SelectObject(dc, of);
        DeleteObject(f);
    }
    GdiFlush();
    let px = std::slice::from_raw_parts(bits, pw * ph).iter().map(|&p| (p >> 16 & 255).max(p >> 8 & 255).max(p & 255) as u8).collect();
    SelectObject(dc, old);
    DeleteDC(dc);
    DeleteObject(bmp);
    px
}

/// Largeur en pixels d'un texte, avec la même police que `text_layer` (position du curseur de saisie).
pub unsafe fn text_width(s: &str, size: f32, weight: i32) -> i32 {
    let dc = CreateCompatibleDC(null_mut());
    let f = font(s, size, weight);
    let of = SelectObject(dc, f);
    let txt: Vec<u16> = s.encode_utf16().collect();
    let mut sz: SIZE = zeroed();
    GetTextExtentPoint32W(dc, txt.as_ptr(), txt.len() as i32, &mut sz);
    SelectObject(dc, of);
    DeleteObject(f);
    DeleteDC(dc);
    sz.cx
}

/// Icône rendue en `n`×`n`, BGRA prémultiplié. Dessinée sur noir puis sur blanc : l'écart donne l'opacité,
/// ce qui marche aussi pour les vieilles icônes à masque (sans canal alpha).
pub unsafe fn icon_pixels(icon: HICON, n: usize) -> Vec<u32> {
    let draw = |bg: u32| {
        let (bmp, bits) = dib(n as i32, n as i32);
        let px = std::slice::from_raw_parts_mut(bits, n * n);
        px.fill(bg);
        let dc = CreateCompatibleDC(null_mut());
        let old = SelectObject(dc, bmp);
        DrawIconEx(dc, 0, 0, icon, n as i32, n as i32, 0, null_mut(), DI_NORMAL);
        GdiFlush();
        let out = px.to_vec();
        SelectObject(dc, old);
        DeleteDC(dc);
        DeleteObject(bmp);
        out
    };
    let (black, white) = (draw(0), draw(0xFF_FFFF));
    black.iter().zip(&white).map(|(&k, &w)| {
        // opacité = 1 − (blanc − noir), sur le canal vert ; la couleur sur noir est déjà prémultipliée
        let a = 255 - ((w >> 8 & 255).saturating_sub(k >> 8 & 255));
        a << 24 | (k & 0xFF_FFFF)
    }).collect()
}

/// Copie de l'écran à cet endroit, réduite `k` fois (le fond sera flouté : la pleine définition ne sert à rien).
/// La fenêtre est toujours cachée à ce moment-là : pas besoin de l'exclure de la capture (ni d'attendre le
/// compositeur avec DwmFlush, ~16 ms gagnées).
pub unsafe fn capture(x: i32, y: i32, w: usize, h: usize, k: usize) -> Back {
    let (cw, ch) = (w / k, h / k);
    let screen = GetDC(null_mut());
    let (bmp, bits) = dib(cw as i32, ch as i32);
    let dc = CreateCompatibleDC(screen);
    let old = SelectObject(dc, bmp);
    SetStretchBltMode(dc, COLORONCOLOR);
    StretchBlt(dc, 0, 0, cw as i32, ch as i32, screen, x, y, w as i32, h as i32, SRCCOPY);
    GdiFlush();
    let px = std::slice::from_raw_parts(bits, cw * ch).iter().map(|&p| {
        let c = unpack(p);
        [c[0], c[1], c[2]]
    }).collect();
    SelectObject(dc, old);
    DeleteDC(dc);
    DeleteObject(bmp);
    ReleaseDC(null_mut(), screen);
    Back { w: cw, h: ch, px, k: w as f32 / cw as f32 }
}

// ---------- zone de notification ----------

/// Logo de l'exe (ressource 1, assets/lens.ico) à la taille des petites icônes : la tuile sombre reste lisible
/// sur une barre des tâches claire comme foncée. Sans la ressource (compilé sans windres), l'icône générique.
pub unsafe fn lens_icon() -> HICON {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    let n = GetSystemMetrics(SM_CXSMICON);
    // 1 comme pointeur = MAKEINTRESOURCE(1)
    let icon = LoadImageW(GetModuleHandleW(null()), 1 as _, IMAGE_ICON, n, n, LR_DEFAULTCOLOR);
    if icon.is_null() { LoadIconW(null_mut(), IDI_APPLICATION) } else { icon as HICON }
}

/// Ajoute ou retire l'icône.
pub unsafe fn tray(hwnd: HWND, msg: u32, op: NOTIFY_ICON_MESSAGE, icon: HICON, tip: &str) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = msg;
    nid.hIcon = icon;
    for (d, c) in nid.szTip.iter_mut().take(127).zip(tip.encode_utf16()) {
        *d = c;
    }
    Shell_NotifyIconW(op, &nid);
}

/// Bulle de notification Windows attachée à l'icône.
pub unsafe fn notify(hwnd: HWND, title: &str, text: &str) -> bool {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_WARNING;
    for (d, c) in nid.szInfoTitle.iter_mut().take(63).zip(title.encode_utf16()) {
        *d = c;
    }
    for (d, c) in nid.szInfo.iter_mut().take(255).zip(text.encode_utf16()) {
        *d = c;
    }
    Shell_NotifyIconW(NIM_MODIFY, &nid) != 0
}

/// Menu contextuel natif (id 0 = séparateur, booléen = coché) ; renvoie l'identifiant choisi (0 si rien).
pub unsafe fn menu(hwnd: HWND, entries: &[(usize, &str, bool)]) -> usize {
    let m = CreatePopupMenu();
    for &(id, label, checked) in entries {
        if id == 0 {
            AppendMenuW(m, MF_SEPARATOR, 0, null());
        } else {
            AppendMenuW(m, MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED }, id, w(label).as_ptr());
        }
    }
    let mut p: POINT = zeroed();
    GetCursorPos(&mut p);
    SetForegroundWindow(hwnd); // sans ça, le menu ne se ferme pas en cliquant ailleurs
    let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, 0, hwnd, null());
    DestroyMenu(m);
    id as usize
}

/// Met du texte dans le presse-papier (résultat d'un calcul, chemin).
pub unsafe fn set_clipboard(hwnd: HWND, text: &str) {
    use windows_sys::Win32::System::{DataExchange::*, Memory::*, Ole::CF_UNICODETEXT};
    let units: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    if OpenClipboard(hwnd) == 0 {
        return;
    }
    EmptyClipboard();
    let h = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
    let p = if h.is_null() { null_mut() } else { GlobalLock(h) as *mut u16 };
    if !p.is_null() {
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        GlobalUnlock(h);
        // le presse-papier devient propriétaire de la mémoire ; on ne la libère que si ça échoue
        if SetClipboardData(CF_UNICODETEXT as u32, h).is_null() {
            GlobalFree(h);
        }
    }
    CloseClipboard();
}

/// Texte du presse-papier (Ctrl+V), une seule ligne.
pub unsafe fn clipboard_text(hwnd: HWND) -> Option<String> {
    use windows_sys::Win32::System::{DataExchange::*, Memory::*, Ole::CF_UNICODETEXT};
    if OpenClipboard(hwnd) == 0 {
        return None;
    }
    let h = GetClipboardData(CF_UNICODETEXT as u32);
    let p = if h.is_null() { null() } else { GlobalLock(h) as *const u16 };
    let out = (!p.is_null()).then(|| {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        GlobalUnlock(h);
        s.lines().next().unwrap_or("").to_string()
    });
    CloseClipboard();
    out
}
