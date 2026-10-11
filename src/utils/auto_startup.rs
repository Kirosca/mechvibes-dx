use std::path::PathBuf;
#[cfg(target_os = "windows")]
use crate::utils::constants::APP_NAME;

#[cfg(target_os = "windows")]
use winreg::enums::*;
#[cfg(target_os = "windows")]
use winreg::RegKey;

#[cfg(target_os = "windows")]
const TASK_NAME: &str = APP_NAME;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Get the current executable path
fn get_exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))
}

#[cfg(target_os = "windows")]
fn run_elevated_schtasks(args: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::GetExitCodeProcess;
    use winapi::um::shellapi::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use winapi::um::synchapi::WaitForSingleObject;
    use winapi::um::winuser::SW_HIDE;

    let prog_wide: Vec<u16> = std::ffi::OsStr::new("schtasks.exe")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let args_wide: Vec<u16> = std::ffi::OsStr::new(args)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb_wide: Vec<u16> = std::ffi::OsStr::new("runas")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: null_mut(),
        lpVerb: verb_wide.as_ptr(),
        lpFile: prog_wide.as_ptr(),
        lpParameters: args_wide.as_ptr(),
        lpDirectory: null_mut(),
        nShow: SW_HIDE,
        hInstApp: null_mut(),
        lpIDList: null_mut(),
        lpClass: null_mut(),
        hkeyClass: null_mut(),
        dwHotKey: 0,
        hMonitor: null_mut(),
        hProcess: null_mut(),
    };

    unsafe {
        if ShellExecuteExW(&mut sei) != 0 && !sei.hProcess.is_null() {
            WaitForSingleObject(sei.hProcess, 10000);
            let mut exit_code: u32 = 0;
            GetExitCodeProcess(sei.hProcess, &mut exit_code);
            CloseHandle(sei.hProcess);
            exit_code == 0
        } else {
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn try_create_scheduled_task(command: &str) -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let status = Command::new("schtasks")
        .args(&[
            "/create",
            "/tn",
            TASK_NAME,
            "/tr",
            command,
            "/sc",
            "onlogon",
            "/rl",
            "highest",
            "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    if let Ok(s) = status {
        if s.success() {
            return true;
        }
    }

    if !crate::utils::platform::is_elevated() {
        let args = format!(
            "/create /tn \"{}\" /tr \"{}\" /sc onlogon /rl highest /f",
            TASK_NAME, command
        );
        return run_elevated_schtasks(&args);
    }

    false
}

#[cfg(target_os = "windows")]
fn try_delete_scheduled_task() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let status = Command::new("schtasks")
        .args(&["/delete", "/tn", TASK_NAME, "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    if let Ok(s) = status {
        if s.success() {
            crate::always_print!("✅ Scheduled task auto-startup deleted");
            return;
        }
    }

    if is_scheduled_task_present() && !crate::utils::platform::is_elevated() {
        let args = format!("/delete /tn \"{}\" /f", TASK_NAME);
        let _ = run_elevated_schtasks(&args);
    }
}

#[cfg(target_os = "windows")]
fn is_scheduled_task_present() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    Command::new("schtasks")
        .args(&["/query", "/tn", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn set_registry_run_entry(command: &str) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey_with_flags("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run", KEY_WRITE)
        .map_err(|e| format!("Failed to open registry key: {}", e))?;

    run_key
        .set_value(APP_NAME, &command)
        .map_err(|e| format!("Failed to set registry value: {}", e))?;

    Ok(())
}

#[cfg(target_os = "windows")]
fn remove_registry_run_entry() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey_with_flags("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run", KEY_WRITE)
        .map_err(|e| format!("Failed to open registry key: {}", e))?;

    match run_key.delete_value(APP_NAME) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to delete registry value: {}", e)),
    }
}

#[cfg(target_os = "windows")]
fn is_registry_run_present() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = match hkcu.open_subkey("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run") {
        Ok(key) => key,
        Err(_) => return false,
    };

    match run_key.get_value::<String, _>(APP_NAME) {
        Ok(value) => {
            let current_exe = get_exe_path().unwrap_or_default();
            let current_exe_str = match current_exe.to_str() {
                Some(path) if !path.is_empty() => path,
                _ => return false,
            };

            exe_path_from_run_command(&value).eq_ignore_ascii_case(current_exe_str)
        }
        Err(_) => false,
    }
}

/// Enable auto startup on Windows (prefers Task Scheduler with highest privileges)
#[cfg(target_os = "windows")]
pub fn enable_auto_startup() -> Result<(), String> {
    let exe_path = get_exe_path()?;
    let exe_path_str = exe_path.to_str().ok_or("Failed to convert executable path to string")?;

    let config = crate::state::config_writer::current();
    let command = if config.start_minimized {
        format!("\"{}\" --minimized", exe_path_str)
    } else {
        exe_path_str.to_string()
    };

    if try_create_scheduled_task(&command) {
        crate::always_print!("✅ Auto startup enabled via Task Scheduler (highest privileges): {}", command);
        let _ = remove_registry_run_entry();
        return Ok(());
    }

    set_registry_run_entry(&command)?;
    crate::always_print!("✅ Auto startup enabled via registry fallback: {}", command);
    Ok(())
}

/// Disable auto startup on Windows
#[cfg(target_os = "windows")]
pub fn disable_auto_startup() -> Result<(), String> {
    try_delete_scheduled_task();
    remove_registry_run_entry()
}

/// Pull the executable path back out of a registry Run command line.
fn exe_path_from_run_command(command: &str) -> &str {
    let command = command.trim();

    if let Some(rest) = command.strip_prefix('"') {
        return match rest.find('"') {
            Some(end) => &rest[..end],
            None => rest,
        };
    }

    command
}

/// Check if auto startup is currently enabled
#[cfg(target_os = "windows")]
pub fn is_auto_startup_enabled() -> bool {
    if is_scheduled_task_present() {
        return true;
    }

    is_registry_run_present()
}

/// Set auto startup state (enable or disable)
pub fn set_auto_startup(_enable: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if _enable { enable_auto_startup() } else { disable_auto_startup() }
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("Auto startup is only supported on Windows".to_string())
    }
}

/// Get current auto startup state
pub fn get_auto_startup_state() -> bool {
    #[cfg(target_os = "windows")]
    {
        is_auto_startup_enabled()
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::exe_path_from_run_command;

    /// Mirrors the unquoted shape `enable_auto_startup` writes when
    /// `start_minimized` is off.
    #[test]
    fn a_bare_path_is_returned_unchanged() {
        let path = r"C:\Program Files\MechvibesDX\mechvibes-dx.exe";
        assert_eq!(exe_path_from_run_command(path), path);
    }

    /// The shape that used to be unrecognizable: with "start minimized" on the
    /// value carries quotes and an argument, and the old whole-string compare
    /// reported "not enabled" for an entry this app had just written.
    #[test]
    fn a_quoted_path_with_the_minimized_flag_yields_just_the_path() {
        let path = r"C:\Program Files\MechvibesDX\mechvibes-dx.exe";
        let command = format!("\"{}\" --minimized", path);
        assert_eq!(exe_path_from_run_command(&command), path);
    }

    /// A quoted path with no arguments must also round-trip, since that is
    /// what a user or an older build may have left behind.
    #[test]
    fn a_quoted_path_without_arguments_yields_just_the_path() {
        let path = r"C:\Apps\mechvibes-dx.exe";
        assert_eq!(exe_path_from_run_command(&format!("\"{}\"", path)), path);
    }

    /// Spaces inside an unquoted path must not be treated as an argument
    /// separator, or every "Program Files" install compares unequal and
    /// self-disables exactly like the quoted bug did.
    #[test]
    fn spaces_in_an_unquoted_path_do_not_truncate_it() {
        let path = r"C:\Program Files\MechvibesDX\mechvibes-dx.exe";
        assert_eq!(exe_path_from_run_command(path), path);
    }

    /// The registry value is written by whatever casing launched the app;
    /// the enabled check compares case-insensitively, so both forms must
    /// reduce to a path that compares equal to the real one.
    #[test]
    fn both_written_forms_match_the_exe_case_insensitively() {
        let exe = r"C:\Program Files\MechvibesDX\mechvibes-dx.exe";
        let bare = r"c:\program files\mechvibesdx\MECHVIBES-DX.EXE";
        let minimized = format!("\"{}\" --minimized", bare);

        assert!(exe_path_from_run_command(bare).eq_ignore_ascii_case(exe));
        assert!(exe_path_from_run_command(&minimized).eq_ignore_ascii_case(exe));
    }

    /// A Run entry pointing at some other program must not be read as "our
    /// auto start is on", or the config sync would flip `auto_start` true.
    #[test]
    fn a_different_executable_does_not_match() {
        let exe = r"C:\Program Files\MechvibesDX\mechvibes-dx.exe";
        let other = "\"C:\\Other\\thing.exe\" --minimized";
        assert!(!exe_path_from_run_command(other).eq_ignore_ascii_case(exe));
    }
}
