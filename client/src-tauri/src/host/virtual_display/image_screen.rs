//! Windows 图片遮罩。复用上游会话租约、防窥状态、重连和锁屏生命周期。
//! 图片仅由被控端托盘导入，本机管道只传图片内容，不接受源文件路径。

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crate::status::StatusHub;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

mod windows;
use windows::Covers;

const MAX_FILE: usize = 8 * 1024 * 1024;
const MAX_PIXELS: u64 = 16_000_000;
const MAX_SIDE: u32 = 2560;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub image_mode: bool,
    pub auto_on: bool,
    pub custom_image: bool,
    pub active: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum Update {
    Mode { image: bool },
    Auto { enabled: bool },
    Image { data: String },
    ResetImage,
    Off,
}

struct Settings {
    options: Options,
    hub: Option<StatusHub>,
}

static SETTINGS: LazyLock<Mutex<Settings>> = LazyLock::new(|| {
    let doc = crate::config::read_text().and_then(|t| t.parse::<toml_edit::DocumentMut>().ok());
    let mut options = Options::default();
    if let Some(doc) = doc {
        options.image_mode = doc.get("privacy_screen").and_then(|v| v.as_str()) == Some("image");
        options.auto_on = doc
            .get("privacy_auto")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        options.custom_image = doc
            .get("privacy_custom_image")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    }
    Mutex::new(Settings { options, hub: None })
});

pub fn init(hub: StatusHub) {
    let mut s = SETTINGS.lock().unwrap();
    s.hub = Some(hub);
    publish(&s);
}

fn publish(s: &Settings) {
    if let Some(hub) = &s.hub {
        hub.send_if_modified(|st| {
            let options = Some(s.options.clone());
            if st.host.image_privacy == options {
                return false;
            }
            st.host.image_privacy = options;
            true
        });
    }
}

pub fn selected() -> bool {
    SETTINGS.lock().unwrap().options.image_mode
}
pub fn auto_on() -> bool {
    let s = SETTINGS.lock().unwrap();
    s.options.image_mode && s.options.auto_on
}
pub fn usable() -> bool {
    windows::supported()
}

pub fn show_error(message: &str) {
    use ::windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use ::windows::core::{HSTRING, w};
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("Airhand"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn image_path() -> Result<PathBuf> {
    Ok(crate::config::path()
        .ok_or_else(|| anyhow!("configuration directory unavailable"))?
        .with_file_name("privacy-image.jpg"))
}

/// 预检格式与像素总量，再解码、缩小并将透明度合成到黑底。
fn normalize(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > MAX_FILE {
        bail!("image must be at most 8 MiB");
    }
    let format = image::guess_format(bytes)?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        bail!("choose a PNG or JPEG image");
    }
    let (w, h) = image::ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        bail!("image must be at most 16 million pixels");
    }
    let decoded = image::load_from_memory_with_format(bytes, format)?;
    let scaled = if w > MAX_SIDE || h > MAX_SIDE {
        decoded.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        decoded
    };
    let rgba = scaled.to_rgba8();
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = u16::from(p[3]);
        image::Rgb([
            (u16::from(p[0]) * a / 255) as u8,
            (u16::from(p[1]) * a / 255) as u8,
            (u16::from(p[2]) * a / 255) as u8,
        ])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&rgb)?;
    if out.len() > MAX_FILE {
        bail!("normalized image is too large");
    }
    Ok(out)
}

pub fn choose_image() -> Result<Option<String>> {
    let Some(path) = windows::choose_file()? else {
        return Ok(None);
    };
    Ok(Some(STANDARD.encode(normalize(&read_image(&path)?)?)))
}

fn read_image(path: &std::path::Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_FILE as u64 {
        bail!("image must be at most 8 MiB");
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(MAX_FILE as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE {
        bail!("image must be at most 8 MiB");
    }
    Ok(bytes)
}

pub fn update(update: Update) -> Result<()> {
    let result = update_inner(update);
    if let Err(e) = &result {
        let mut s = SETTINGS.lock().unwrap();
        s.options.error = Some(e.to_string());
        publish(&s);
    }
    result
}

fn update_inner(update: Update) -> Result<()> {
    if matches!(update, Update::Off) {
        super::local_off();
        return Ok(());
    }
    if matches!(update, Update::Mode { image: true }) && !usable() {
        bail!("image privacy needs Windows 10 version 2004 or newer");
    }
    let reset_image = matches!(update, Update::ResetImage);
    {
        let w = super::HUB.wants.lock().unwrap();
        if !w.sessions.is_empty() || w.privacy(std::time::Instant::now()) {
            bail!("disconnect all controllers before changing privacy screen settings");
        }
    }
    let imported = match &update {
        Update::Image { data } => {
            if data.len() > MAX_FILE * 4 / 3 + 8 {
                bail!("image is too large");
            }
            Some(normalize(
                &STANDARD.decode(data).context("invalid image encoding")?,
            )?)
        }
        _ => None,
    };
    super::shutdown();
    let mut w = super::HUB.wants.lock().unwrap();
    if w.running || !w.sessions.is_empty() {
        bail!("privacy manager is busy; try again");
    }
    let mut s = SETTINGS.lock().unwrap();
    let mut options = s.options.clone();
    match update {
        Update::Mode { image } => options.image_mode = image,
        Update::Auto { enabled } => options.auto_on = enabled,
        Update::Image { .. } => {
            let path = image_path()?;
            let tmp = path.with_extension("jpg.tmp");
            std::fs::write(&tmp, imported.as_ref().unwrap())?;
            std::fs::rename(&tmp, &path)?;
            options.custom_image = true;
        }
        Update::ResetImage => options.custom_image = false,
        Update::Off => unreachable!(),
    }
    crate::config::save_image_privacy(options.image_mode, options.auto_on, options.custom_image)?;
    if reset_image {
        let path = image_path()?;
        for file in [&path, &path.with_extension("jpg.tmp")] {
            if let Err(e) = std::fs::remove_file(file) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("can't remove the discarded privacy image: {e}");
                }
            }
        }
    }
    options.error = None;
    s.options = options;
    publish(&s);
    drop(s);
    w.stopping = false;
    super::publish(&w);
    Ok(())
}

pub(super) fn spawn() -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("airhand-image-privacy".into())
        .spawn(run)
        .map(|_| ())
}

fn run() {
    let custom = SETTINGS.lock().unwrap().options.custom_image;
    let picture = if custom {
        match image_path()
            .and_then(|p| read_image(&p))
            .and_then(|b| normalize(&b))
        {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                let mut s = SETTINGS.lock().unwrap();
                s.options.error = Some(format!("saved image unavailable; using solid black: {e}"));
                publish(&s);
                None
            }
        }
    } else {
        None
    };
    let mut covers: Option<Covers> = None;
    let mut blocker = None;
    loop {
        let (on, stopping) = {
            let w = super::HUB.wants.lock().unwrap();
            (w.privacy(std::time::Instant::now()), w.stopping)
        };
        if stopping {
            break;
        }
        if on {
            let result = if let Some(c) = &mut covers {
                c.refresh()
            } else {
                Covers::new(picture.as_deref()).map(|c| {
                    covers = Some(c);
                })
            };
            if let Err(e) = result {
                super::fail(format!("image privacy screen: {e:#}"));
                covers = None;
                blocker = None;
            } else if blocker.is_none() {
                blocker = Some(super::block::Blocker::start());
            }
        } else {
            blocker = None;
            covers = None;
        }
        windows::pump();
        let active = covers.is_some();
        {
            let mut w = super::HUB.wants.lock().unwrap();
            w.on = active;
            super::publish(&w);
        }
        {
            let mut s = SETTINGS.lock().unwrap();
            s.options.active = active;
            publish(&s);
        }
        let w = super::HUB.wants.lock().unwrap();
        let _ = super::HUB
            .wake
            .wait_timeout(w, Duration::from_millis(100))
            .unwrap();
    }
    drop(blocker);
    drop(covers);
    let mut w = super::HUB.wants.lock().unwrap();
    w.on = false;
    w.running = false;
    super::publish(&w);
    super::HUB.wake.notify_all();
    drop(w);
    let mut s = SETTINGS.lock().unwrap();
    s.options.active = false;
    publish(&s);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transparent_png_imports_on_black_and_rejects_invalid_data() {
        let rgba = image::RgbaImage::from_pixel(20, 10, image::Rgba([255, 255, 255, 0]));
        let mut bytes = Cursor::new(Vec::new());
        rgba.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let jpeg = normalize(bytes.get_ref()).unwrap();
        let result = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(result.dimensions(), (20, 10));
        assert!(result.pixels().all(|p| p.0.iter().all(|v| *v <= 2)));
        assert!(normalize(&vec![0; MAX_FILE + 1]).is_err());
        assert!(normalize(b"<svg/>").is_err());
    }
    #[test]
    fn huge_dimensions_are_rejected_before_decoding() {
        let image = image::GrayImage::new(5000, 4000);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        assert!(
            normalize(bytes.get_ref())
                .unwrap_err()
                .to_string()
                .contains("16 million")
        );
    }

    #[test]
    #[ignore = "briefly covers the physical screens and blocks local input"]
    fn native_manager_keeps_other_holders_and_restores_after_release() {
        use airhand_proto::session::PrivacyState;
        use std::time::Instant;
        struct Restore(Options);
        impl Drop for Restore {
            fn drop(&mut self) {
                super::super::local_off();
                super::super::shutdown();
                SETTINGS.lock().unwrap().options = self.0.clone();
            }
        }
        let _restore = {
            let mut s = SETTINGS.lock().unwrap();
            let old = s.options.clone();
            s.options = Options {
                image_mode: true,
                ..Default::default()
            };
            Restore(old)
        };
        fn wait(state: PrivacyState) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let p = super::super::HUB.privacy.borrow().state;
                if p == state {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "expected {state:?}, actual {p:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let a = super::super::join(88001);
        let b = super::super::join(88002);
        a.set_privacy(true);
        b.set_privacy(true);
        wait(PrivacyState::On);
        assert!(SETTINGS.lock().unwrap().options.active);
        drop(a);
        assert_eq!(super::super::HUB.privacy.borrow().state, PrivacyState::On);
        drop(b);
        super::super::release_privacy();
        wait(PrivacyState::Off);
        super::super::shutdown();
        assert!(!SETTINGS.lock().unwrap().options.active);
        assert!(!super::super::HUB.wants.lock().unwrap().running);
    }
}
