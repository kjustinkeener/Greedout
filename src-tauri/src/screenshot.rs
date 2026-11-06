//! Self-only window capture for the pipe `screenshot` verb (`control.rs`).
//!
//! Deliberately takes an `HWND`, never a raw pointer/PID string from the wire: callers must go
//! through `control.rs`'s label lookup (`app.get_webview_window(label)`, restricted to
//! `control::ALL_WINDOWS`), so this module can only ever be pointed at a window Greedout itself
//! owns. There is no "capture any window" path here on purpose: `PrintWindow` renders a window's
//! own content into an off-screen bitmap rather than reading the screen, so it is not caught by
//! OS/policy screen-capture restrictions and it cannot pick up another app's pixels even if that
//! app is drawn on top - unless the code itself hands it someone else's HWND, which this API
//! shape rules out.
//!
//! Output is PNG: small enough to return over the control pipe (a full-window BMP is tens of MB
//! of raw pixels and blows past the MCP transport's per-call limit) and directly viewable.

use windows::core::BOOL;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// Size of a `BITMAPINFOHEADER`, for `BITMAPINFO::bmiHeader.biSize` below (a DIB section still
/// needs this even though the output is PNG, not BMP).
const INFO_HEADER_SIZE: u32 = 40;

/// Render into a bitmap without asking the window to erase its background first - avoids a
/// visible flash and matches what a normal repaint looks like.
const PW_RENDERFULLCONTENT: u32 = 0x00000002;

// `PrintWindow` (user32.dll) is absent from the `windows`/`windows-sys` crates' generated
// bindings (checked both at the pinned versions here) even though it is a plain, long-stable
// Win32 API - declared directly rather than pulling in a third crate for one function.
#[link(name = "user32")]
extern "system" {
    fn PrintWindow(hwnd: HWND, hdc_blt: HDC, flags: u32) -> BOOL;
}

/// Render `hwnd`'s own content (not whatever is on top of it on screen) into a PNG file's bytes.
pub fn capture_hwnd(hwnd: HWND) -> Result<Vec<u8>, String> {
    let mut rect = Default::default();
    unsafe { GetClientRect(hwnd, &mut rect) }.map_err(|e| format!("GetClientRect: {e}"))?;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return Err(format!("window has no visible client area ({width}x{height})"));
    }

    unsafe {
        let window_dc = GetDC(Some(hwnd));
        if window_dc.is_invalid() {
            return Err("GetDC failed".into());
        }
        let mem_dc = CreateCompatibleDC(Some(window_dc));
        if mem_dc.is_invalid() {
            ReleaseDC(Some(hwnd), window_dc);
            return Err("CreateCompatibleDC failed".into());
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: INFO_HEADER_SIZE,
            biWidth: width,
            // Negative height: a top-down DIB, so the pixel buffer below comes out in the same
            // row order PrintWindow paints in, matching what PNG expects (row 0 = top).
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0 as u32,
            ..Default::default()
        };

        let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
            .map_err(|e| format!("CreateDIBSection: {e}"))?;
        if dib.is_invalid() || bits_ptr.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(Some(hwnd), window_dc);
            return Err("CreateDIBSection returned no pixel buffer".into());
        }

        let old_obj = SelectObject(mem_dc, HGDIOBJ(dib.0));
        let painted = PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT);

        let row_bytes = (width as usize) * 4;
        let pixels = if painted.as_bool() {
            let src = std::slice::from_raw_parts(bits_ptr as *const u8, row_bytes * height as usize);
            Some(src.to_vec())
        } else {
            None
        };

        SelectObject(mem_dc, old_obj);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(Some(hwnd), window_dc);

        let top_down = pixels.ok_or_else(|| "PrintWindow failed to render the window".to_string())?;
        to_png(width as u32, height as u32, &top_down)
    }
}

/// PNG-encode a top-down 32bpp BGRA buffer. Alpha is forced opaque: GDI's capture buffer has no
/// meaningful alpha channel for an ordinary (non-layered) window, and leaving it as-is can produce
/// a partially or fully transparent-looking PNG.
fn to_png(width: u32, height: u32, top_down_bgra: &[u8]) -> Result<Vec<u8>, String> {
    let mut rgba = top_down_bgra.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2); // BGRA -> RGBA
        px[3] = 255;
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| format!("png encode: {e}"))?;
    }
    Ok(out)
}
