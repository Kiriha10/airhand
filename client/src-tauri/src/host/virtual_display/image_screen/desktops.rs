//! 普通桌面和 Winlogon 各有独立线程及窗口，先准备安全桌面，再准备普通桌面。
//! HWND 不能跟着输入桌面搬迁；UAC 时保留两边窗口，不清除会话的防窥请求。

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ::windows::Win32::Foundation::{GENERIC_ALL, HWND};
use ::windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, GetThreadDesktop, HDESK, OpenDesktopW, SetThreadDesktop,
};
use ::windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use ::windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use ::windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, MsgWaitForMultipleObjects, PostThreadMessageW, QS_ALLINPUT, WM_NULL,
};
use ::windows::core::HSTRING;
use anyhow::{Context, Result, anyhow, bail};

use super::cursor::{HiddenCursor, Runtime};
use super::windows::{self, Covers};

const REFRESH: Duration = Duration::from_millis(100);
thread_local! {
    static LAYERS_CHANGED: Cell<bool> = const { Cell::new(false) };
}

struct Attachment {
    desk: HDESK,
    original: HDESK,
}

impl Attachment {
    fn new(name: &str) -> Result<Self> {
        unsafe {
            let original = GetThreadDesktop(GetCurrentThreadId())?;
            let desk = OpenDesktopW(
                &HSTRING::from(name),
                DESKTOP_CONTROL_FLAGS(0),
                false,
                GENERIC_ALL.0,
            )
            .with_context(|| format!("cannot prepare the {name} privacy desktop"))?;
            if let Err(e) = SetThreadDesktop(desk) {
                let _ = CloseDesktop(desk);
                return Err(e)
                    .with_context(|| format!("cannot attach to the {name} privacy desktop"));
            }
            Ok(Self { desk, original })
        }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        // 这里执行时属于本线程的窗口、钩子已销毁，恢复线程原桌面后才能关闭句柄。
        unsafe {
            let _ = SetThreadDesktop(self.original);
            let _ = CloseDesktop(self.desk);
        }
    }
}

unsafe extern "system" fn layer_event(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != 0 && pid != unsafe { GetCurrentProcessId() } {
        // OUTOFCONTEXT 的回调在安装钩子的线程执行；不在回调内改层级，避免重入。
        LAYERS_CHANGED.set(true);
    }
}

struct LayerEvents(Vec<HWINEVENTHOOK>);

impl LayerEvents {
    fn new() -> Result<Self> {
        let mut me = Self(Vec::new());
        // 前台窗口、窗口显示/隐藏/重排、位置改变。自进程事件在回调中忽略。
        for (first, last) in [(3, 3), (0x8002, 0x8004), (0x800b, 0x800b)] {
            let hook = unsafe { SetWinEventHook(first, last, None, Some(layer_event), 0, 0, 0) };
            if hook.is_invalid() {
                bail!("cannot monitor privacy window layers");
            }
            me.0.push(hook);
        }
        Ok(me)
    }
}

impl Drop for LayerEvents {
    fn drop(&mut self) {
        for hook in self.0.drain(..) {
            let _ = unsafe { UnhookWinEvent(hook) };
        }
    }
}

struct Worker {
    name: &'static str,
    stop: Arc<AtomicBool>,
    thread_id: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    initial_windows: Vec<usize>,
}

impl Worker {
    fn start(
        name: &'static str,
        picture: Arc<Option<Vec<u8>>>,
        runtime: Arc<Runtime>,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::new(AtomicU32::new(0));
        let error = Arc::new(Mutex::new(None));
        let (s, t, e) = (stop.clone(), thread_id.clone(), error.clone());
        let (ready, receive) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name(format!("airhand-privacy-{name}"))
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let _desktop = Attachment::new(name)?;
                    t.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
                    let mut covers = Covers::new(picture.as_ref().as_deref())?;
                    let _cursor = HiddenCursor::new(runtime)?;
                    let _events = LayerEvents::new()?;
                    ready
                        .send(Ok(covers.handles()))
                        .map_err(|_| anyhow!("privacy start was cancelled"))?;
                    let mut refreshed = Instant::now();
                    while !s.load(Ordering::Acquire) {
                        windows::pump();
                        if LAYERS_CHANGED.replace(false) || refreshed.elapsed() >= REFRESH {
                            covers.refresh()?;
                            refreshed = Instant::now();
                        }
                        let timeout = REFRESH.saturating_sub(refreshed.elapsed());
                        unsafe {
                            MsgWaitForMultipleObjects(
                                None,
                                false,
                                timeout.as_millis() as u32,
                                QS_ALLINPUT,
                            );
                        }
                    }
                    Ok(())
                })();
                if let Err(err) = result {
                    let message = format!("{name} image privacy: {err:#}");
                    *e.lock().unwrap() = Some(message.clone());
                    let _ = ready.send(Err(message));
                }
            })?;
        let mut me = Self {
            name,
            stop,
            thread_id,
            error,
            thread: Some(thread),
            #[cfg(test)]
            initial_windows: Vec::new(),
        };
        let handles = receive
            .recv_timeout(Duration::from_secs(3))
            .with_context(|| format!("{name} privacy desktop did not become ready"))?
            .map_err(|err| anyhow!(err))?;
        tracing::info!(
            desktop = name,
            displays = handles.len(),
            "image privacy desktop prepared"
        );
        #[cfg(test)]
        {
            me.initial_windows = handles;
        }
        Ok(me)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::Win32::Foundation::HWND;
    use ::windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use ::windows::Win32::UI::WindowsAndMessaging::{
        GetWindowDisplayAffinity, IsWindow, IsWindowVisible, WDA_EXCLUDEFROMCAPTURE,
    };
    use ::windows::core::{BOOL, s, w};

    fn validate(covers: &DesktopCovers) -> Vec<HWND> {
        let mut handles = Vec::new();
        assert_eq!(covers.workers.len(), 2);
        for worker in &covers.workers {
            assert!(!worker.initial_windows.is_empty());
            for &raw in &worker.initial_windows {
                let hwnd = HWND(raw as *mut _);
                unsafe {
                    assert!(
                        IsWindowVisible(hwnd).as_bool(),
                        "{} window is not visible",
                        worker.name
                    );
                    let mut affinity = 0;
                    GetWindowDisplayAffinity(hwnd, &mut affinity).unwrap();
                    assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE.0);
                    let f = GetProcAddress(
                        GetModuleHandleW(w!("user32.dll")).unwrap(),
                        s!("GetWindowBand"),
                    )
                    .unwrap();
                    let f: unsafe extern "system" fn(HWND, *mut u32) -> BOOL =
                        std::mem::transmute(f);
                    let mut band = 0;
                    assert!(f(hwnd, &mut band).as_bool());
                    assert_eq!(band, 2, "{} is not in the UIAccess band", worker.name);
                    println!(
                        "{} cover: visible, UIAccess band={band}, affinity=0x{affinity:x}",
                        worker.name
                    );
                }
                handles.push(hwnd);
            }
        }
        handles
    }

    #[test]
    #[ignore = "requires a SYSTEM/UIAccess test process and briefly covers both desktops"]
    fn native_prepared_desktops_keep_covers_and_restore() {
        let mut covers = DesktopCovers::new(None).unwrap();
        let handles = validate(&covers);
        covers.refresh().unwrap();
        drop(covers);
        assert!(
            handles
                .iter()
                .all(|h| !unsafe { IsWindow(Some(*h)) }.as_bool())
        );
        assert!(!super::super::physical_cursor_hidden());
    }

    fn cancel_uac() {
        if !crate::host::desktop::input_is_secure() {
            return;
        }
        std::thread::spawn(|| {
            use ::windows::Win32::UI::Input::KeyboardAndMouse::*;
            assert!(crate::host::desktop::follow());
            let inputs = [KEYBD_EVENT_FLAGS(0), KEYEVENTF_KEYUP].map(|flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_ESCAPE,
                        dwFlags: flags,
                        dwExtraInfo: crate::host::input::INJECTED,
                        ..Default::default()
                    },
                },
            });
            assert_eq!(unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) }, 2);
        })
        .join()
        .unwrap();
    }

    #[test]
    #[ignore = "requires the native harness to request and automatically cancel a real UAC prompt"]
    fn native_uac_keeps_covers_and_remote_escape() {
        let ready = std::env::var("AIRHAND_TEST_UAC_READY")
            .expect("run through the native privacy harness");
        let mut covers = DesktopCovers::new(None).unwrap();
        let handles = validate(&covers);
        let blocker = crate::host::virtual_display::block::Blocker::start();
        struct Cancel;
        impl Drop for Cancel {
            fn drop(&mut self) {
                cancel_uac();
            }
        }
        let _cancel = Cancel;
        std::fs::write(&ready, b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !crate::host::desktop::input_is_secure() {
            assert!(
                Instant::now() < deadline,
                "the harness did not trigger a secure UAC desktop"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        covers.refresh().unwrap();
        validate(&covers);
        cancel_uac();
        let deadline = Instant::now() + Duration::from_secs(5);
        while crate::host::desktop::input_is_secure() {
            assert!(
                Instant::now() < deadline,
                "remote Escape did not cancel UAC"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        covers.refresh().unwrap();
        validate(&covers);
        println!(
            "REAL UAC VERIFIED: covers prepared before switch, kept on Winlogon and after return, tagged remote Escape accepted"
        );
        drop(blocker);
        drop(covers);
        assert!(
            handles
                .iter()
                .all(|h| !unsafe { IsWindow(Some(*h)) }.as_bool())
        );
        assert!(!super::super::physical_cursor_hidden());
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let id = self.thread_id.load(Ordering::Acquire);
        if id != 0 {
            let _ =
                unsafe { PostThreadMessageW(id, WM_NULL, Default::default(), Default::default()) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) struct DesktopCovers {
    workers: Vec<Worker>,
    // 必须晚于所有桌面线程退出；Runtime 内部负责 MagUninitialize。
    _runtime: Arc<Runtime>,
}

impl DesktopCovers {
    pub fn new(bytes: Option<&[u8]>) -> Result<Self> {
        if !windows::has_ui_access() {
            bail!(
                "image privacy needs UIAccess; install Airhand for all users and restart its service"
            );
        }
        let runtime = Runtime::new()?;
        let picture = Arc::new(bytes.map(<[u8]>::to_vec));
        let mut me = Self {
            workers: Vec::new(),
            _runtime: runtime.clone(),
        };
        for name in ["Winlogon", "Default"] {
            me.workers
                .push(Worker::start(name, picture.clone(), runtime.clone())?);
        }
        Ok(me)
    }

    pub fn refresh(&mut self) -> Result<()> {
        for worker in &self.workers {
            if let Some(error) = worker.error.lock().unwrap().clone() {
                bail!("{error}");
            }
            if worker
                .thread
                .as_ref()
                .is_none_or(|thread| thread.is_finished())
            {
                bail!("{} privacy desktop stopped", worker.name);
            }
        }
        Ok(())
    }
}
