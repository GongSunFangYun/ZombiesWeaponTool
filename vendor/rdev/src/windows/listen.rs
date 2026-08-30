use crate::rdev::{Event, ListenError};
use crate::windows::common::{convert, set_key_hook, set_mouse_hook, HookError, HOOK};
use std::os::raw::c_int;
use std::ptr::null_mut;
use std::time::SystemTime;
use winapi::shared::minwindef::{LPARAM, LRESULT, WPARAM};
use winapi::um::winuser::{CallNextHookEx, GetMessageA, HC_ACTION};

static mut GLOBAL_CALLBACK: Option<Box<dyn FnMut(Event)>> = None;

impl From<HookError> for ListenError {
    fn from(error: HookError) -> Self {
        match error {
            HookError::Mouse(code) => ListenError::MouseHookError(code),
            HookError::Key(code) => ListenError::KeyHookError(code),
        }
    }
}

// 注意：此处的本地补丁去掉了对每个 KeyPress 调用 Keyboard::get_name 的逻辑。
// get_name 在低层键盘钩子线程里做 AttachThreadInput/GetKeyboardState/ToUnicodeEx，
// 前台窗口（如游戏）繁忙时会阻塞钩子线程 → Windows 跳过后续事件 → KeyUp 丢失 → 按键卡住。
// Event.name 本程序不使用，直接置 None，钩子回调保持极快。

unsafe extern "system" fn raw_callback(code: c_int, param: WPARAM, lpdata: LPARAM) -> LRESULT {
    if code == HC_ACTION {
        if let Some(event_type) = convert(param, lpdata) {
            let event = Event {
                event_type,
                time: SystemTime::now(),
                name: None,
            };
            if let Some(callback) = &mut GLOBAL_CALLBACK {
                callback(event);
            }
        }
    }
    CallNextHookEx(HOOK, code, param, lpdata)
}

pub fn listen<T>(callback: T) -> Result<(), ListenError>
where
    T: FnMut(Event) + 'static,
{
    unsafe {
        GLOBAL_CALLBACK = Some(Box::new(callback));
        set_key_hook(raw_callback)?;
        set_mouse_hook(raw_callback)?;

        GetMessageA(null_mut(), null_mut(), 0, 0);
    }
    Ok(())
}
