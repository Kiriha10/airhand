use ::windows::Win32::Foundation::CloseHandle;
use ::windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use ::windows::Win32::Graphics::Dwm::{
    DWMWA_DISALLOW_PEEK, DWMWA_EXCLUDED_FROM_PEEK, DwmSetWindowAttribute,
};
use ::windows::Win32::Graphics::Gdi::*;
use ::windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenUIAccess};
use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
use ::windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use ::windows::Win32::UI::WindowsAndMessaging::*;
use ::windows::core::{BOOL, w};
use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

#[repr(C)]
struct Version {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    text: [u16; 128],
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version: *mut Version) -> i32;
}

/// 旧版 Windows 将 EXCLUDEFROMCAPTURE 降为捕获黑块，不能当作可用。
pub(super) fn supported() -> bool {
    let mut v = Version {
        size: size_of::<Version>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        text: [0; 128],
    };
    unsafe { RtlGetVersion(&mut v) >= 0 && (v.major > 10 || (v.major == 10 && v.build >= 19041)) }
}

pub(super) fn has_ui_access() -> bool {
    static ACCESS: OnceLock<bool> = OnceLock::new();
    *ACCESS.get_or_init(|| unsafe {
        let mut token = Default::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut access = 0u32;
        let mut needed = 0;
        let result = GetTokenInformation(
            token,
            TokenUIAccess,
            Some((&mut access as *mut u32).cast()),
            size_of::<u32>() as u32,
            &mut needed,
        );
        let _ = CloseHandle(token);
        result.is_ok() && access != 0
    })
}

#[derive(Clone)]
struct Picture {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
}
struct Window {
    hwnd: HWND,
    _picture: Arc<Option<Picture>>,
}
impl Drop for Window {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = &*(lp.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                FillRect(dc, &rect, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Option<Picture>;
                if let Some(Some(p)) = ptr.as_ref() {
                    let (x, y, width, height) = fit(p.width, p.height, rect.right, rect.bottom);
                    let info = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: p.width,
                            biHeight: -p.height,
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    SetStretchBltMode(dc, HALFTONE);
                    StretchDIBits(
                        dc,
                        x,
                        y,
                        width,
                        height,
                        0,
                        0,
                        p.width,
                        p.height,
                        Some(p.pixels.as_ptr().cast()),
                        &info,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    );
                }
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wp, lp)
    }
}

fn fit(w: i32, h: i32, target_w: i32, target_h: i32) -> (i32, i32, i32, i32) {
    let factor = (target_w as f64 / w as f64).min(target_h as f64 / h as f64);
    let width = (w as f64 * factor).round() as i32;
    let height = (h as f64 * factor).round() as i32;
    (
        (target_w - width) / 2,
        (target_h - height) / 2,
        width,
        height,
    )
}

fn monitors() -> Result<Vec<RECT>> {
    unsafe extern "system" fn each(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
        unsafe {
            (&mut *(data.0 as *mut Vec<RECT>)).push(*rect);
        }
        BOOL(1)
    }
    let mut out = Vec::new();
    if !unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut out as *mut _ as isize)) }
        .as_bool()
    {
        bail!("cannot enumerate displays");
    }
    if out.is_empty() {
        bail!("no physical screen to cover");
    }
    Ok(out)
}

pub(super) struct Covers {
    windows: Vec<Window>,
    rects: Vec<RECT>,
    picture: Arc<Option<Picture>>,
}
impl Covers {
    pub(super) fn handles(&self) -> Vec<usize> {
        self.windows.iter().map(|w| w.hwnd.0 as usize).collect()
    }

    pub fn new(bytes: Option<&[u8]>) -> Result<Self> {
        if !supported() {
            bail!("image privacy needs Windows 10 version 2004 or newer");
        }
        let picture = bytes
            .map(|b| -> Result<Picture> {
                let mut image = image::load_from_memory(b)?.to_rgba8();
                for p in image.pixels_mut() {
                    p.0.swap(0, 2);
                }
                Ok(Picture {
                    width: image.width() as i32,
                    height: image.height() as i32,
                    pixels: image.into_raw(),
                })
            })
            .transpose()?;
        let mut me = Self {
            windows: Vec::new(),
            rects: Vec::new(),
            picture: Arc::new(picture),
        };
        me.refresh()?;
        Ok(me)
    }

    pub fn refresh(&mut self) -> Result<()> {
        // 每个桌面由独立的遮罩线程维护。非活动桌面的窗口也必须保留，
        // UAC 切换时目标桌面已经有遮罩，不能在这里销毁普通桌面的窗口。
        let rects = crate::host::dpi_aware(monitors)?;
        if self.rects != rects {
            let mut windows = Vec::new();
            for rect in &rects {
                windows.push(self.create(rect)?);
            }
            // 新显示器全部创建成功再撤掉旧窗口。
            self.windows = windows;
            self.rects = rects;
        }
        for window in &self.windows {
            unsafe {
                // Win+D 等系统操作可能隐藏窗口，下一轮恢复可见状态。
                let _ = ShowWindow(window.hwnd, SW_SHOWNOACTIVATE);
                SetWindowPos(
                    window.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )?;
            }
        }
        pump();
        Ok(())
    }

    fn create(&self, rect: &RECT) -> Result<Window> {
        static CLASS: OnceLock<Result<(), String>> = OnceLock::new();
        let module = unsafe { GetModuleHandleW(None)? };
        CLASS
            .get_or_init(|| {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(procedure),
                    hInstance: HINSTANCE(module.0),
                    lpszClassName: w!("AirhandImagePrivacy"),
                    ..Default::default()
                };
                if unsafe { RegisterClassW(&class) } == 0 {
                    Err(::windows::core::Error::from_thread().to_string())
                } else {
                    Ok(())
                }
            })
            .as_ref()
            .map_err(|e| anyhow!("{e}"))?;
        let hwnd = crate::host::dpi_aware(|| unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                w!("AirhandImagePrivacy"),
                w!("Airhand privacy screen"),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(HINSTANCE(module.0)),
                Some(Arc::as_ptr(&self.picture).cast()),
            )
        })?;
        let window = Window {
            hwnd,
            _picture: self.picture.clone(),
        };
        unsafe {
            // Aero Peek 会绕过应用设置的 Alpha，把其他窗口渐隐。
            // 在显示前设置，API 失败则拒绝开启。这两个属性只支持 DwmSet，
            // DwmGet 会返回 E_INVALIDARG，不能用它读回验证。
            let yes = BOOL(1);
            for attr in [DWMWA_EXCLUDED_FROM_PEEK, DWMWA_DISALLOW_PEEK] {
                DwmSetWindowAttribute(
                    hwnd,
                    attr,
                    (&yes as *const BOOL).cast(),
                    size_of::<BOOL>() as u32,
                )?;
            }
            SetLayeredWindowAttributes(hwnd, Default::default(), 255, LWA_ALPHA)?;
            SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)?;
            let mut affinity = 0;
            GetWindowDisplayAffinity(hwnd, &mut affinity)?;
            if affinity != WDA_EXCLUDEFROMCAPTURE.0 {
                bail!("capture exclusion was not applied");
            }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = UpdateWindow(hwnd);
        }
        Ok(window)
    }
}

pub(super) fn pump() {
    let mut msg = MSG::default();
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub(super) fn choose_file() -> Result<Option<PathBuf>> {
    use ::windows::Win32::System::Com::*;
    use ::windows::Win32::UI::Shell::*;
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR | FOS_FORCEFILESYSTEM,
        )?;
        let filter = [Common::COMDLG_FILTERSPEC {
            pszName: w!("PNG / JPEG"),
            pszSpec: w!("*.png;*.jpg;*.jpeg"),
        }];
        dialog.SetFileTypes(&filter)?;
        match dialog.Show(None) {
            Err(e) if e.code().0 == 0x800704c7u32 as i32 => return Ok(None),
            result => result?,
        }
        let item = dialog.GetResult()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(name.to_string()?);
        CoTaskMemFree(Some(name.0.cast()));
        Ok(Some(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aspect_fit_leaves_opaque_black_bars() {
        assert_eq!(fit(200, 100, 100, 100), (0, 25, 100, 50));
        assert_eq!(fit(100, 200, 100, 100), (25, 0, 50, 100));
    }

    /// 区分隐藏物理光标是否也会从 DXGI 原始画面移除软件光标，不能仅看可见性标志。
    #[test]
    #[ignore = "briefly shows a small reference window and restores the physical cursor"]
    fn native_physical_cursor_capture_pixels() {
        use crate::host::capture;
        use ::windows::Win32::UI::WindowsAndMessaging::{CURSORINFO, GetCursorInfo};
        use std::time::{Duration, Instant};
        let mut ci = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        unsafe { GetCursorInfo(&mut ci) }.unwrap();
        let d = capture::displays()
            .into_iter()
            .find(|d| {
                (d.x..d.x + d.width as i32).contains(&ci.ptScreenPos.x)
                    && (d.y..d.y + d.height as i32).contains(&ci.ptScreenPos.y)
            })
            .expect("cursor is on a real display");
        let rect = RECT {
            left: (ci.ptScreenPos.x - 60).max(d.x),
            top: (ci.ptScreenPos.y - 60).max(d.y),
            right: (ci.ptScreenPos.x + 100).min(d.x + d.width as i32),
            bottom: (ci.ptScreenPos.y + 100).min(d.y + d.height as i32),
        };
        fn solid(bgr: [u8; 3]) -> Arc<Option<Picture>> {
            Arc::new(Some(Picture {
                pixels: [bgr[0], bgr[1], bgr[2], 255].repeat(64 * 64),
                width: 64,
                height: 64,
            }))
        }
        let base = Covers {
            windows: Vec::new(),
            rects: Vec::new(),
            picture: solid([180, 20, 210]),
        };
        let mut reference = base.create(&rect).unwrap();
        unsafe { SetWindowDisplayAffinity(reference.hwnd, WDA_NONE) }.unwrap();
        let sub = capture::subscribe(d.index, 30, 0, true);
        let get = |expect: [u8; 3]| {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                pump();
                if let Some((_, f)) = sub.slot().try_newer(0, false) {
                    let p = f.pixels().unwrap();
                    let at = (((rect.top - d.y + 3) as u32 * f.width
                        + (rect.left - d.x + 3) as u32)
                        * 4) as usize;
                    if p[at..at + 3]
                        .iter()
                        .zip(expect)
                        .all(|(a, b)| a.abs_diff(b) < 5)
                    {
                        return f;
                    }
                }
                assert!(Instant::now() < deadline, "reference frame did not update");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let count = |f: &crate::host::capture::Frame, bgr: [u8; 3]| {
            let p = f.pixels().unwrap();
            let mut n = 0;
            for y in (ci.ptScreenPos.y - 4).max(rect.top + 3)
                ..(ci.ptScreenPos.y + 48).min(rect.bottom - 3)
            {
                for x in (ci.ptScreenPos.x - 4).max(rect.left + 3)
                    ..(ci.ptScreenPos.x + 48).min(rect.right - 3)
                {
                    let at = (((y - d.y) as u32 * f.width + (x - d.x) as u32) * 4) as usize;
                    if p[at..at + 3]
                        .iter()
                        .zip(bgr)
                        .any(|(a, b)| a.abs_diff(b) > 20)
                    {
                        n += 1;
                    }
                }
            }
            n
        };
        let before = get([180, 20, 210]);
        let before_pixels = count(&before, [180, 20, 210]);
        let before_state = sub.slot().cursor().visibility;
        let hidden = super::super::hide_test_cursor().unwrap();
        reference._picture = solid([30, 200, 40]);
        unsafe {
            SetWindowLongPtrW(
                reference.hwnd,
                GWLP_USERDATA,
                Arc::as_ptr(&reference._picture) as isize,
            );
            let _ = InvalidateRect(Some(reference.hwnd), None, true);
            let _ = UpdateWindow(reference.hwnd);
        }
        let during = get([30, 200, 40]);
        let during_pixels = count(&during, [30, 200, 40]);
        let during_state = sub.slot().cursor().visibility;
        let drawn = sub
            .slot()
            .try_newer(0, true)
            .expect("frame with logical remote cursor")
            .1;
        let drawn_pixels = count(&drawn, [30, 200, 40]);
        let a = drawn.pixels().unwrap();
        let b = during.pixels().unwrap();
        let mut changed = Vec::new();
        for (at, (a, b)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
            if a[..3] != b[..3] {
                changed.push((at % drawn.width as usize, at / drawn.width as usize));
            }
        }
        drop(hidden);
        println!(
            "raw DXGI cursor pixels before={before_pixels} ({before_state:?}), hidden={during_pixels} ({during_state:?}), remote-drawn={drawn_pixels}; changed={}, first={:?}, cursor={:?}",
            changed.len(),
            changed.first(),
            ci.ptScreenPos
        );
        if ci.flags.0 & CURSOR_SHOWING.0 != 0 {
            assert_eq!(
                during_state,
                airhand_proto::session::CursorVisibility::Shown
            );
            assert!(
                !changed.is_empty(),
                "remote draw_cursor must still include the logical cursor"
            );
        }
    }

    /// 真正的物理显示器 / DXGI 集成验证；只短暂创建窗口，不改变驱动、配置或显示拓扑。
    #[test]
    #[ignore = "briefly covers the physical screens with a test image"]
    fn physical_image_is_excluded_from_live_dxgi_and_windows_are_restored() {
        use crate::host::capture;
        use std::time::{Duration, Instant};
        assert!(supported());
        let displays = capture::displays();
        let d = displays.first().expect("a real display");
        fn solid(bgr: [u8; 3]) -> Arc<Option<Picture>> {
            Arc::new(Some(Picture {
                pixels: [bgr[0], bgr[1], bgr[2], 255].repeat(64 * 64),
                width: 64,
                height: 64,
            }))
        }
        let rect = RECT {
            left: d.x + 60,
            top: d.y + 60,
            right: d.x + 380,
            bottom: d.y + 380,
        };
        let base = Covers {
            windows: Vec::new(),
            rects: Vec::new(),
            picture: solid([180, 20, 210]),
        };
        let mut reference = base.create(&rect).unwrap();
        unsafe {
            SetWindowDisplayAffinity(reference.hwnd, WDA_NONE).unwrap();
        }
        pump();
        let sub = capture::subscribe(d.index, 30, 0, false);
        fn wait_color(sub: &capture::Subscription, expect: [u8; 3]) -> [u8; 3] {
            let until = Instant::now() + Duration::from_secs(5);
            let mut last = [0; 3];
            loop {
                pump();
                if let Some((_, frame)) = sub.slot().try_newer(0, false) {
                    let pixels = frame.pixels().unwrap();
                    let at = ((160 * frame.width + 160) * 4) as usize;
                    last.copy_from_slice(&pixels[at..at + 3]);
                    if last.iter().zip(expect).all(|(a, b)| a.abs_diff(b) <= 3) {
                        return last;
                    }
                }
                assert!(
                    Instant::now() < until,
                    "DXGI expected {expect:?}, got {last:?}; error {:?}",
                    sub.slot().error()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let before = wait_color(&sub, [180, 20, 210]);
        let image = image::RgbImage::from_pixel(200, 200, image::Rgb([20, 80, 240]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&image)
            .unwrap();
        let mut covers = Covers::new(Some(&jpeg)).unwrap();
        covers.refresh().unwrap();
        pump();
        for window in &covers.windows {
            assert!(unsafe { IsWindowVisible(window.hwnd) }.as_bool());
            let mut affinity = 0;
            unsafe {
                GetWindowDisplayAffinity(window.hwnd, &mut affinity).unwrap();
            }
            assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE.0);
        }
        let during = wait_color(&sub, [180, 20, 210]);
        unsafe {
            let _ = ShowWindow(covers.windows[0].hwnd, SW_HIDE);
        }
        covers.refresh().unwrap();
        assert!(unsafe { IsWindowVisible(covers.windows[0].hwnd) }.as_bool());
        reference._picture = solid([30, 200, 40]);
        unsafe {
            SetWindowLongPtrW(
                reference.hwnd,
                GWLP_USERDATA,
                Arc::as_ptr(&reference._picture) as isize,
            );
            let _ = InvalidateRect(Some(reference.hwnd), None, true);
            let _ = UpdateWindow(reference.hwnd);
        }
        let live = wait_color(&sub, [30, 200, 40]);
        let handles: Vec<_> = covers.windows.iter().map(|w| w.hwnd).collect();
        drop(covers);
        pump();
        assert!(
            handles
                .iter()
                .all(|h| !unsafe { IsWindow(Some(*h)) }.as_bool())
        );
        let after = wait_color(&sub, [30, 200, 40]);
        println!(
            "physical covers visible + capture excluded; DXGI before={before:?}, covered={during:?}, live-update={live:?}, restored={after:?}"
        );
    }
}
