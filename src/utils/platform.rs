/// Platform-related utility functions

/// Get the current platform identifier
pub fn get_platform() -> String {
    if cfg!(target_os = "windows") {
        "windows".to_string()
    } else if cfg!(target_os = "macos") {
        "macos".to_string()
    } else if cfg!(target_os = "linux") {
        "linux".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Get build type (debug or release)
pub fn get_build_type() -> String {
    if cfg!(debug_assertions) {
        "debug".to_string()
    } else {
        "release".to_string()
    }
}

/// Get supported architectures for the current platform
pub fn get_supported_architectures() -> Vec<String> {
    vec!["x86_64".to_string()]
}

/// Get minimum OS version requirement
pub fn get_min_os_version() -> String {
    if cfg!(target_os = "windows") {
        "Windows 10".to_string()
    } else if cfg!(target_os = "macos") {
        "macOS 10.14".to_string()
    } else if cfg!(target_os = "linux") {
        "Ubuntu 18.04".to_string()
    } else {
        "Unknown".to_string()
    }
}

/// Check if the current process is running with administrative privileges.
pub fn is_elevated() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::mem::size_of;
        use std::ptr::null_mut;
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::GetCurrentProcess;
        use winapi::um::securitybaseapi::GetTokenInformation;
        use winapi::um::winnt::{HANDLE, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};

        unsafe {
            let mut token: HANDLE = null_mut();
            if winapi::um::processthreadsapi::OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY,
                &mut token,
            ) == 0
            {
                return false;
            }

            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut return_length: u32 = 0;
            let success = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut _,
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut return_length,
            );

            CloseHandle(token);

            success != 0 && elevation.TokenIsElevated != 0
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Restart the current application with administrator privileges via "runas" (Windows).
pub fn restart_as_administrator() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null_mut;
        use winapi::um::shellapi::ShellExecuteW;
        use winapi::um::winuser::SW_SHOWNORMAL;

        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable path: {}", e))?;
        let exe_wide: Vec<u16> = exe_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let verb_wide: Vec<u16> = std::ffi::OsStr::new("runas")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let h_instance = unsafe {
            ShellExecuteW(
                null_mut(),
                verb_wide.as_ptr(),
                exe_wide.as_ptr(),
                null_mut(),
                null_mut(),
                SW_SHOWNORMAL,
            )
        };

        if (h_instance as usize) > 32 {
            crate::always_print!("🚀 Successfully requested elevated restart via ShellExecuteW");
            Ok(())
        } else {
            Err(format!(
                "Failed to spawn elevated process (code: {})",
                h_instance as usize
            ))
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("Restart as administrator is only supported on Windows".to_string())
    }
}

