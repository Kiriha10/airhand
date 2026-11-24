//! MagShowSystemCursor(FALSE) 使 DXGI 报硬件光标不可见，但 GetCursorInfo 仍报显示中，
//! 且原始画面并不包含光标。隐私屏期间从 Win32 获取逻辑光标，继续给远端正常的
//! Shown/Hidden、形状及带光标画面；游戏自己隐藏光标时仍然是 Hidden。

use std::sync::Arc;

use airhand_proto::session::{CursorShape, CursorVisibility};
use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::{CursorBitmap, convert_pointer, pointer_bitmap};

#[derive(Default)]
pub(super) struct Cursor {
    handle: usize,
    hotspot: (i32, i32),
    bitmap: Option<Arc<CursorBitmap>>,
}

pub(super) struct Observation {
    pub visibility: CursorVisibility,
    pub here: bool,
    pub pos: (i32, i32),
    pub bitmap: Option<Arc<CursorBitmap>>,
    pub shape: Option<CursorShape>,
}

impl Cursor {
    pub fn observe(&mut self, desktop: &RECT) -> Observation {
        let mut ci = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        let mut out = Observation {
            visibility: CursorVisibility::Hidden,
            here: true,
            pos: (0, 0),
            bitmap: None,
            shape: None,
        };
        if unsafe { GetCursorInfo(&mut ci) }.is_err()
            || ci.flags.0 & CURSOR_SHOWING.0 == 0
            || ci.flags.0 & CURSOR_SUPPRESSED.0 != 0
            || ci.hCursor.is_invalid()
        {
            return out;
        }
        let key = ci.hCursor.0 as usize;
        if key != self.handle || self.bitmap.is_none() {
            match read_cursor(ci.hCursor) {
                Ok((bitmap, shape, hotspot)) => {
                    self.handle = key;
                    self.hotspot = hotspot;
                    self.bitmap = Some(Arc::new(bitmap));
                    out.shape = Some(shape);
                }
                Err(error) => {
                    tracing::debug!("cannot read the privacy cursor: {error:#}");
                    self.handle = 0;
                    self.bitmap = None;
                }
            }
        }
        out.visibility = CursorVisibility::Shown;
        out.here = (desktop.left..desktop.right).contains(&ci.ptScreenPos.x)
            && (desktop.top..desktop.bottom).contains(&ci.ptScreenPos.y);
        out.pos = (
            ci.ptScreenPos.x - desktop.left - self.hotspot.0,
            ci.ptScreenPos.y - desktop.top - self.hotspot.1,
        );
        out.bitmap = self.bitmap.clone();
        out
    }
}

struct IconBitmaps(ICONINFO);
impl Drop for IconBitmaps {
    fn drop(&mut self) {
        for bitmap in [self.0.hbmColor, self.0.hbmMask] {
            if !bitmap.is_invalid() {
                let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
            }
        }
    }
}

fn pixels(bitmap: HBITMAP) -> Result<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut desc = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some((&mut desc as *mut BITMAP).cast()),
        ) == 0
        {
            bail!("GetObject(cursor bitmap)");
        }
        let (width, height) = (desc.bmWidth, desc.bmHeight);
        if width <= 0 || height <= 0 || width > 256 || height > 512 {
            bail!("cursor bitmap dimensions are out of range");
        }
        let dc = GetDC(None);
        if dc.is_invalid() {
            bail!("GetDC(cursor bitmap)");
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0; (width * height * 4) as usize];
        let rows = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(data.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        if rows != height {
            bail!("GetDIBits(cursor bitmap)");
        }
        Ok((width as u32, height as u32, data))
    }
}

fn read_cursor(cursor: HCURSOR) -> Result<(CursorBitmap, CursorShape, (i32, i32))> {
    let mut icon = ICONINFO::default();
    unsafe { GetIconInfo(HICON(cursor.0), &mut icon) }.context("GetIconInfo")?;
    let icon = IconBitmaps(icon);
    let (width, height, pitch, kind, data) = if icon.0.hbmColor.is_invalid() {
        let (w, h, mask) = pixels(icon.0.hbmMask)?;
        if h % 2 != 0 {
            bail!("invalid monochrome cursor mask");
        }
        let pitch = w.div_ceil(32) * 4;
        let mut bits = vec![0u8; (pitch * h) as usize];
        for y in 0..h {
            for x in 0..w {
                if mask[((y * w + x) * 4) as usize] != 0 {
                    bits[(y * pitch + x / 8) as usize] |= 0x80 >> (x % 8);
                }
            }
        }
        (
            w,
            h,
            pitch,
            DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
            bits,
        )
    } else {
        let (w, h, mut color) = pixels(icon.0.hbmColor)?;
        if h > 256 {
            bail!("cursor is too tall");
        }
        let kind = if color.chunks_exact(4).any(|p| p[3] != 0) {
            DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR
        } else {
            let (mw, mh, mask) = pixels(icon.0.hbmMask)?;
            if (mw, mh) != (w, h) {
                bail!("cursor mask dimensions do not match");
            }
            for (p, m) in color.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                p[3] = if m[0] != 0 { 255 } else { 0 };
            }
            DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR
        };
        (w, h, w * 4, kind, color)
    };
    let hotspot = (icon.0.xHotspot as i32, icon.0.yHotspot as i32);
    let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
        Type: kind.0 as u32,
        Width: width,
        Height: height,
        Pitch: pitch,
        HotSpot: windows::Win32::Foundation::POINT {
            x: hotspot.0,
            y: hotspot.1,
        },
    };
    let bitmap = pointer_bitmap(&data, &info).context("invalid Win32 cursor bitmap")?;
    let shape = convert_pointer(&data, &info).context("invalid Win32 cursor shape")?;
    Ok((bitmap, shape, hotspot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_win32_arrow_text_and_cross_cursors_convert_for_remote_rendering() {
        for id in [IDC_ARROW, IDC_IBEAM, IDC_CROSS] {
            let cursor = unsafe { LoadCursorW(None, id) }.unwrap();
            let (bitmap, shape, _) = read_cursor(cursor).unwrap();
            assert!(bitmap.valid());
            assert!(shape.width > 0 && shape.height > 0);
            assert_eq!(
                shape.rgba.len(),
                shape.width as usize * shape.height as usize * 4
            );
            assert!(
                shape.rgba.chunks_exact(4).any(|p| p[3] != 0),
                "system cursor must contain visible pixels"
            );
        }
    }
}
