//! Locates the desktop app without a disk-wide scan or a fixed package version.

use std::io;
use std::path::{Path, PathBuf};

const APP_EXE: &str = "1Password.exe";

#[cfg(test)]
pub(super) static START_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn version(text: &str) -> Option<[u32; 4]> {
    let mut result = [0; 4];
    for (index, part) in text.split('.').enumerate() {
        *result.get_mut(index)? = part.parse().ok()?;
    }
    Some(result)
}

fn package_version(name: &str) -> Option<[u32; 4]> {
    let mut parts = name.split('_');
    if !parts.next()?.eq_ignore_ascii_case("Agilebits.1Password") {
        return None;
    }
    version(parts.next()?)
}

fn newest_app(candidates: impl IntoIterator<Item = ([u32; 4], PathBuf)>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .filter(|(_, path)| path.is_absolute() && path.is_file())
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

/// Only the known app folder and one level of version directories are queried.
fn classic_app(root: &Path) -> Option<PathBuf> {
    let direct = root.join(APP_EXE);
    if direct.is_file() {
        return Some(direct);
    }
    let app = root.join("app");
    let direct = app.join(APP_EXE);
    if direct.is_file() {
        return Some(direct);
    }
    newest_app(std::fs::read_dir(app).ok()?.filter_map(|entry| {
        let entry = entry.ok()?;
        let version = entry
            .file_name()
            .to_str()
            .and_then(version)
            .unwrap_or_default();
        Some((version, entry.path().join(APP_EXE)))
    }))
}

pub(super) fn detect() -> io::Result<Option<PathBuf>> {
    let registered = registered_app();
    if let Ok(Some(path)) = &registered {
        return Ok(Some(path.clone()));
    }
    let mut roots = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        roots.push(local.join("1Password"));
        roots.push(local.join("Programs").join("1Password"));
    }
    for variable in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable).map(PathBuf::from) {
            let path = root.join("1Password");
            if !roots.contains(&path) {
                roots.push(path);
            }
        }
    }
    if let Some(path) = roots
        .iter()
        .filter(|root| root.is_absolute())
        .find_map(|root| classic_app(root))
    {
        return Ok(Some(path));
    }
    registered
}

#[cfg(windows)]
fn registered_app() -> io::Result<Option<PathBuf>> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
        RegOpenKeyExW,
    };
    use windows_sys::core::w;

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: this wrapper owns a successfully opened registry handle.
            unsafe { RegCloseKey(self.0) };
        }
    }

    let mut handle = std::ptr::null_mut();
    // SAFETY: the key is a static terminated string and the output handle is writable.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(
                "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages"
            ),
            0,
            KEY_READ,
            &mut handle,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let key = Key(handle);
    let mut candidates = Vec::new();
    for index in 0.. {
        // A registry key component is at most 255 UTF-16 characters.
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        // SAFETY: the live key and writable name/length buffers remain valid for this call.
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut length,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let text = String::from_utf16_lossy(&name[..length as usize]);
        let Some(version) = package_version(&text) else {
            continue;
        };
        let mut bytes = 0;
        // SAFETY: the subkey is terminated by the zero-initialized buffer; only size is queried.
        let status = unsafe {
            RegGetValueW(
                key.0,
                name.as_ptr(),
                w!("PackageRootFolder"),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            continue;
        }
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let mut folder = vec![0u16; bytes as usize / 2];
        // SAFETY: folder has the byte capacity returned by the size query; pointers stay live.
        let status = unsafe {
            RegGetValueW(
                key.0,
                name.as_ptr(),
                w!("PackageRootFolder"),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                folder.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let length = folder
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(folder.len());
        let path = PathBuf::from(OsString::from_wide(&folder[..length])).join(APP_EXE);
        candidates.push((version, path));
    }
    Ok(newest_app(candidates))
}

#[cfg(not(windows))]
fn registered_app() -> io::Result<Option<PathBuf>> {
    Ok(None)
}

pub(super) fn start() -> io::Result<bool> {
    let Some(path) = detect()? else {
        return Ok(false);
    };
    launch(&path)?;
    #[cfg(test)]
    START_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(true)
}

#[cfg(windows)]
fn launch(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    use windows_sys::Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
    };
    use windows_sys::core::w;
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            // SAFETY: this guard balances a successful initialization on this thread.
            unsafe { CoUninitialize() };
        }
    }
    // SAFETY: no reserved pointer is used and this scope balances a successful call.
    let initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if initialized < 0 && initialized != RPC_E_CHANGED_MODE {
        return Err(io::Error::other(format!(
            "Could not initialize app launching ({initialized:#x})."
        )));
    }
    // An existing apartment is already initialized and must not be uninitialized here.
    let _com = if initialized >= 0 { Some(Com) } else { None };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: strings are terminated and remain live; no window, arguments or directory is passed.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            w!("open"),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as usize;
    if result <= 32 {
        return Err(io::Error::other(format!(
            "1Password app launch failed (shell code {result})."
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "The desktop app requires Windows.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("codex-hosts-desktop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        root
    }

    fn app_at(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture").unwrap();
    }

    #[test]
    fn msix_identity_accepts_version_changes_and_excludes_the_cli() {
        assert_eq!(
            package_version("Agilebits.1Password_19.4.38.7_x64__publisher"),
            Some([19, 4, 38, 7])
        );
        assert_eq!(
            package_version("agilebits.1password_8.99.0.1_arm64__other"),
            Some([8, 99, 0, 1])
        );
        assert_eq!(
            package_version("AgileBits.1Password.CLI_2.41.1_x64__publisher"),
            None
        );
        assert_eq!(
            package_version("Other.1Password_19.4.38.7_x64__publisher"),
            None
        );
    }

    #[test]
    fn classic_search_is_shallow_and_orders_versions_numerically() {
        let root = fixture();
        app_at(&root.join("app/9.9.0/1Password.exe"));
        let newest = root.join("app/10.2.1/1Password.exe");
        app_at(&newest);
        app_at(&root.join("app/99.0.0/nested/1Password.exe"));
        assert_eq!(classic_app(&root), Some(newest));
        app_at(&root.join(APP_EXE));
        assert_eq!(classic_app(&root), Some(root.join(APP_EXE)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_or_non_app_candidates_do_not_hide_an_installed_version() {
        let root = fixture();
        let existing = root.join("one/1Password.exe");
        app_at(&existing);
        let missing = root.join("two/1Password.exe");
        assert_eq!(
            newest_app([([8, 1, 0, 0], existing.clone()), ([9, 0, 0, 0], missing)]),
            Some(existing)
        );
        assert_eq!(classic_app(&root), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
