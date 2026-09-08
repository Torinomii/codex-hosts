//! Runtime-only notification-area icon. No startup registration or persistent state.
use crate::i18n::Catalog;
use eframe::egui;
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
const ICON_SIZE: usize = 32;
const MASK_STRIDE: usize = 4;

fn pixel_offset(x: usize, y: usize) -> usize {
    ((ICON_SIZE - 1 - y) * ICON_SIZE + x) * 4
}

fn set_pixel(pixels: &mut [u8], x: usize, y: usize, bgra: [u8; 4]) {
    let offset = pixel_offset(x, y);
    pixels[offset..offset + 4].copy_from_slice(&bgra);
}

fn tray_icon_bitmaps() -> (Vec<u8>, Vec<u8>) {
    let mut pixels = vec![0_u8; ICON_SIZE * ICON_SIZE * 4];
    let mut mask = vec![0xff_u8; ICON_SIZE * MASK_STRIDE];

    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as i32 * 2 + 1 - ICON_SIZE as i32;
            let dy = y as i32 * 2 + 1 - ICON_SIZE as i32;
            if dx * dx + dy * dy <= 28 * 28 {
                set_pixel(&mut pixels, x, y, [216, 114, 37, 255]);
                let row = ICON_SIZE - 1 - y;
                mask[row * MASK_STRIDE + x / 8] &= !(1 << (7 - x % 8));
            }
        }
    }

    for step in 0..8 {
        for thickness in 0..2 {
            set_pixel(&mut pixels, 8 + step, 8 + step + thickness, [255; 4]);
            set_pixel(&mut pixels, 15 - step, 16 + step + thickness, [255; 4]);
        }
    }
    for y in 22..24 {
        for x in 17..25 {
            set_pixel(&mut pixels, x, y, [255; 4]);
        }
    }
    (pixels, mask)
}

fn create_tray_icon() -> io::Result<HICON> {
    let (pixels, mask) = tray_icon_bitmaps();
    let handle = unsafe {
        CreateIcon(
            std::ptr::null_mut(),
            ICON_SIZE as i32,
            ICON_SIZE as i32,
            1,
            32,
            mask.as_ptr(),
            pixels.as_ptr(),
        )
    };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(handle)
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
    fn tray_icon_has_transparent_exterior_and_visible_terminal_mark() {
        let (pixels, mask) = tray_icon_bitmaps();
        assert_eq!(pixels.len(), 32 * 32 * 4);
        assert_eq!(mask.len(), 32 * 4);
        assert_eq!(
            &pixels[pixel_offset(0, 0)..pixel_offset(0, 0) + 4],
            &[0, 0, 0, 0]
        );
        assert_ne!(
            &pixels[pixel_offset(16, 16)..pixel_offset(16, 16) + 3],
            &[0, 0, 0]
        );
        assert_eq!(
            &pixels[pixel_offset(20, 22)..pixel_offset(20, 22) + 3],
            &[255, 255, 255]
        );
        assert_ne!(mask.iter().copied().fold(0_u8, |all, byte| all | byte), 0);
        assert_ne!(
            mask.iter().copied().fold(0xff_u8, |all, byte| all & byte),
            0xff
        );
    }

    #[test]
    fn owned_tray_icon_is_a_valid_windows_handle() {
        let icon = create_tray_icon().unwrap();
        assert!(!icon.is_null());
        assert_ne!(unsafe { DestroyIcon(icon) }, 0);
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
