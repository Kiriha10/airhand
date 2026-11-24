//! 隐藏物理系统光标，不调用 ShowCursor，不改变应用自己的光标显示计数。
//! Magnification 初始化按进程计数，隐藏/恢复在各自桌面的遮罩线程执行。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ::windows::Win32::UI::Magnification::{MagInitialize, MagShowSystemCursor, MagUninitialize};
use anyhow::{Result, bail};

static USERS: Mutex<usize> = Mutex::new(0);
static HIDDEN: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn physically_hidden() -> bool {
    HIDDEN.load(Ordering::Acquire) != 0
}

pub(super) struct Runtime;

impl Runtime {
    pub fn new() -> Result<Arc<Self>> {
        let mut users = USERS.lock().unwrap();
        if *users == 0 && !unsafe { MagInitialize() }.as_bool() {
            bail!("cannot initialize privacy cursor protection");
        }
        *users += 1;
        Ok(Arc::new(Self))
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let mut users = USERS.lock().unwrap();
        *users -= 1;
        if *users == 0 && !unsafe { MagUninitialize() }.as_bool() {
            tracing::warn!("cannot uninitialize privacy cursor protection");
        }
    }
}

pub(super) struct HiddenCursor {
    _runtime: Arc<Runtime>,
}

impl HiddenCursor {
    pub fn new(runtime: Arc<Runtime>) -> Result<Self> {
        if !unsafe { MagShowSystemCursor(false) }.as_bool() {
            bail!("cannot hide the physical system cursor");
        }
        HIDDEN.fetch_add(1, Ordering::AcqRel);
        Ok(Self { _runtime: runtime })
    }
}

impl Drop for HiddenCursor {
    fn drop(&mut self) {
        if !unsafe { MagShowSystemCursor(true) }.as_bool() {
            tracing::warn!("cannot restore the physical system cursor");
        }
        HIDDEN.fetch_sub(1, Ordering::AcqRel);
    }
}
