//! Runtime-only notification-area icon. No startup registration or persistent state.
use crate::i18n::Catalog;
use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::RefCell;
use std::io;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::*, WindowsAndMessaging::*},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Open,
    Secrets,
    Exit,
    Unavailable,
}
fn action(id: usize) -> Option<Event> {
    match id {
        1 => Some(Event::Open),
        2 => Some(Event::Secrets),
        3 => Some(Event::Exit),
        _ => None,
    }
}
const CALLBACK: u32 = WM_APP + 1;
/// `build.rs` embeds `assets/icon.ico` under this resource ID.
const APP_ICON_RESOURCE: u16 = 1;

/// Loads the embedded application icon at one system icon size, so Windows
/// picks the matching frame instead of scaling one bitmap.
fn load_app_icon(
    width_metric: SYSTEM_METRICS_INDEX,
    height_metric: SYSTEM_METRICS_INDEX,
) -> io::Result<HICON> {
    let handle = unsafe {
        LoadImageW(
            GetModuleHandleW(std::ptr::null()),
            APP_ICON_RESOURCE as usize as *const u16,
            IMAGE_ICON,
            GetSystemMetrics(width_metric),
            GetSystemMetrics(height_metric),
            LR_DEFAULTCOLOR,
        )
    };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(handle as HICON)
    }
}

fn create_tray_icon() -> io::Result<HICON> {
    load_app_icon(SM_CXSMICON, SM_CYSMICON)
}

/// Gives the window the icon's own frames for its title bar and taskbar button, which
/// would otherwise be scaled down from the 256-pixel window icon and lose their sharpness.
/// The handles stay valid for the window's life, which ends with the process.
pub fn use_embedded_window_icon(window: &impl HasWindowHandle) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    for (kind, width, height) in [
        (ICON_SMALL, SM_CXSMICON, SM_CYSMICON),
        (ICON_BIG, SM_CXICON, SM_CYICON),
    ] {
        if let Ok(icon) = load_app_icon(width, height) {
            unsafe {
                SendMessageW(
                    handle.hwnd.get() as HWND,
                    WM_SETICON,
                    kind as usize,
                    icon as isize,
                );
            }
        }
    }
}

struct State {
    sender: mpsc::SyncSender<Event>,
    context: egui::Context,
    catalog: Arc<Mutex<Catalog>>,
    taskbar: u32,
    icon: HICON,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn emit(event: Event) {
    STATE.with(|state| {
        if let Some(state) = state.borrow().as_ref() {
            let _ = state.sender.try_send(event);
            state.context.request_repaint();
        }
    });
}
fn icon(hwnd: HWND, handle: HICON) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: CALLBACK,
        hIcon: handle,
        ..Default::default()
    };
    let tip = wide("Codex Hosts");
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: handles belong to this message thread. State is only immutably borrowed and cloned
    // before entering reentrant native menu APIs. No secret-bearing objects cross this boundary.
    unsafe {
        let recreated_icon = STATE.with(|state| {
            state.borrow().as_ref().and_then(|state| {
                (state.taskbar != 0 && msg == state.taskbar).then_some(state.icon)
            })
        });
        if let Some(handle) = recreated_icon {
            if Shell_NotifyIconW(NIM_ADD, &icon(hwnd, handle)) == 0 {
                emit(Event::Unavailable);
            }
            return 0;
        }
        match msg {
            CALLBACK => {
                if l as u32 == WM_LBUTTONUP {
                    emit(Event::Open);
                }
                if l as u32 == WM_RBUTTONUP {
                    let labels = STATE.with(|s| {
                        s.borrow().as_ref().and_then(|s| {
                            s.catalog.lock().ok().map(|c| {
                                [
                                    wide(c.text("tray_open")),
                                    wide(c.text("temp_title")),
                                    wide(c.text("tray_exit")),
                                ]
                            })
                        })
                    });
                    if let Some(labels) = labels {
                        let menu = CreatePopupMenu();
                        if !menu.is_null() {
                            let mut ok = true;
                            for (index, label) in labels.iter().enumerate() {
                                ok &= AppendMenuW(menu, MF_STRING, index + 1, label.as_ptr()) != 0;
                            }
                            if ok {
                                let mut point = POINT::default();
                                GetCursorPos(&mut point);
                                SetForegroundWindow(hwnd);
                                let selected = TrackPopupMenu(
                                    menu,
                                    TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                                    point.x,
                                    point.y,
                                    0,
                                    hwnd,
                                    std::ptr::null(),
                                );
                                if selected > 0 {
                                    PostMessageW(hwnd, WM_COMMAND, selected as usize, 0);
                                }
                                PostMessageW(hwnd, WM_NULL, 0, 0);
                            }
                            DestroyMenu(menu);
                        }
                    }
                }
                0
            }
            WM_COMMAND => {
                if let Some(event) = action(w & 0xffff) {
                    emit(event);
                }
                0
            }
            WM_QUERYENDSESSION => 1,
            WM_ENDSESSION => {
                if w != 0 {
                    emit(Event::Exit);
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}

pub struct Tray {
    hwnd: usize,
    worker: Option<thread::JoinHandle<()>>,
    receiver: mpsc::Receiver<Event>,
    catalog: Arc<Mutex<Catalog>>,
}
impl Tray {
    pub fn new(context: egui::Context, catalog: Catalog) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(8);
        let catalog = Arc::new(Mutex::new(catalog));
        let shared = catalog.clone();
        let (ready, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || unsafe {
            let class = wide(&format!("CodexHostsTray-{}", std::process::id()));
            let instance = GetModuleHandleW(std::ptr::null());
            let tray_icon = match create_tray_icon() {
                Ok(icon) => icon,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            let definition = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hIcon: tray_icon,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            if RegisterClassW(&definition) == 0 {
                let error = io::Error::last_os_error();
                DestroyIcon(tray_icon);
                let _ = ready.send(Err(error));
                return;
            }
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("Codex Hosts Tray").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                let error = io::Error::last_os_error();
                UnregisterClassW(class.as_ptr(), instance);
                DestroyIcon(tray_icon);
                let _ = ready.send(Err(error));
                return;
            }
            STATE.with(|state| {
                *state.borrow_mut() = Some(State {
                    sender,
                    context,
                    catalog: shared,
                    taskbar: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                    icon: tray_icon,
                })
            });
            if Shell_NotifyIconW(NIM_ADD, &icon(hwnd, tray_icon)) == 0 {
                STATE.with(|state| *state.borrow_mut() = None);
                DestroyWindow(hwnd);
                UnregisterClassW(class.as_ptr(), instance);
                DestroyIcon(tray_icon);
                let _ = ready.send(Err(io::Error::other("tray unavailable")));
                return;
            }
            if ready.send(Ok(hwnd as usize)).is_ok() {
                let mut message = MSG::default();
                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            Shell_NotifyIconW(NIM_DELETE, &icon(hwnd, tray_icon));
            if IsWindow(hwnd) != 0 {
                DestroyWindow(hwnd);
            }
            UnregisterClassW(class.as_ptr(), instance);
            emit(Event::Unavailable);
            STATE.with(|state| *state.borrow_mut() = None);
            DestroyIcon(tray_icon);
        });
        match result.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(hwnd)) => Ok(Self {
                hwnd,
                worker: Some(worker),
                receiver,
                catalog,
            }),
            other => {
                drop(worker); // A late startup sees its receiver closed and removes the icon.
                Err(other
                    .err()
                    .map(|_| io::Error::other("tray thread stopped"))
                    .unwrap_or_else(|| io::Error::other("tray unavailable")))
            }
        }
    }
    pub fn poll(&self) -> Option<Event> {
        self.receiver.try_recv().ok()
    }
    pub fn set_catalog(&self, catalog: &Catalog) {
        if let Ok(mut current) = self.catalog.lock() {
            *current = catalog.clone();
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tray_icon_is_the_embedded_colour_icon_at_small_icon_size() {
        use windows_sys::Win32::Graphics::Gdi::{BITMAP, DeleteObject, GetObjectW};
        let icon = create_tray_icon().unwrap();
        unsafe {
            let mut info = ICONINFO::default();
            assert_ne!(GetIconInfo(icon, &mut info), 0);
            assert!(!info.hbmColor.is_null(), "a colour icon, not a mask only");
            let mut bitmap = BITMAP::default();
            assert_ne!(
                GetObjectW(
                    info.hbmColor,
                    std::mem::size_of::<BITMAP>() as i32,
                    (&mut bitmap as *mut BITMAP).cast(),
                ),
                0
            );
            assert_eq!(bitmap.bmWidth, GetSystemMetrics(SM_CXSMICON));
            assert_eq!(bitmap.bmHeight, GetSystemMetrics(SM_CYSMICON));
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
            assert_ne!(DestroyIcon(icon), 0);
        }
    }

    /// Every frame reaches all four canvas edges and its light racks span most of the width,
    /// because the dark hexagon alone nearly disappears on a dark taskbar.
    #[test]
    fn every_icon_frame_fills_its_canvas() {
        let ico = include_bytes!("../assets/icon.ico");
        let field = |at: usize, len: usize| {
            ico[at..at + len]
                .iter()
                .rev()
                .fold(0_usize, |value, byte| value << 8 | usize::from(*byte))
        };
        let count = field(4, 2);
        let mut sizes = Vec::new();
        for entry in 0..count {
            let entry = 6 + 16 * entry;
            let (length, offset) = (field(entry + 8, 4), field(entry + 12, 4));
            let frame = eframe::icon_data::from_png_bytes(&ico[offset..offset + length]).unwrap();
            sizes.push(frame.width);
            assert_eq!(frame.width, frame.height);
            assert_frame_fills_canvas(&frame);
        }
        for size in [16, 20, 24, 32, 256] {
            assert!(sizes.contains(&size), "missing the {size}-pixel frame");
        }
        assert_frame_fills_canvas(
            &eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).unwrap(),
        );
    }

    fn assert_frame_fills_canvas(frame: &egui::IconData) {
        let size = frame.width as usize;
        let pixel = |x: usize, y: usize| &frame.rgba[(y * size + x) * 4..(y * size + x) * 4 + 4];
        let last = size - 1;
        for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
            assert_eq!(
                pixel(x, y)[3],
                0,
                "{size}px corner {x},{y} stays transparent"
            );
        }
        let middle = size / 2;
        for (x, y) in [(middle, 0), (middle, last), (0, middle), (last, middle)] {
            assert!(
                pixel(x, y)[3] >= 128,
                "{size}px does not reach edge {x},{y}"
            );
        }
        let light = |x: usize, y: usize| pixel(x, y)[1] > 180;
        let columns = (0..size)
            .filter(|&x| (0..size).any(|y| light(x, y)))
            .collect::<Vec<_>>();
        let span = columns[columns.len() - 1] - columns[0] + 1;
        assert!(
            span as f32 >= 0.65 * size as f32,
            "{size}px racks span only {span} pixels"
        );
    }

    #[test]
    fn exit_menu_labels_are_concise() {
        for (locale, expected) in [
            ("en", "Exit"),
            ("ja", "終了"),
            ("zh-CN", "退出"),
            ("zh-TW", "退出"),
        ] {
            assert_eq!(
                Catalog::for_locale(Some(locale)).text("tray_exit"),
                expected
            );
        }
    }

    #[test]
    fn only_explicit_exit_action_exits() {
        assert_eq!(action(1), Some(Event::Open));
        assert_eq!(action(2), Some(Event::Secrets));
        assert_eq!(action(3), Some(Event::Exit));
        assert_eq!(action(0), None);
    }
}
