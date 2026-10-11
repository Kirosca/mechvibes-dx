//! Stops a second copy of the app from running (Windows).
//!
//! Two UI processes means two sets of input listeners feeding two audio
//! engines, so every keystroke plays twice. That was true of the old rdev
//! listeners too, but the Raw Input worker makes it easier to hit: the
//! worker captures regardless of focus, so a forgotten second instance is
//! audible everywhere rather than only when that window is in the
//! background.
//!
//! Uses a named mutex rather than a lock file because Windows releases it
//! automatically when the process dies, however it dies - a lock file left
//! behind by a crash would keep the app from ever starting again.
#![cfg(target_os = "windows")]

use std::collections::hash_map::DefaultHasher;
use std::hash::{ Hash, Hasher };
use std::ptr::null_mut;

use winapi::um::errhandlingapi::GetLastError;
use winapi::um::handleapi::CloseHandle;
use winapi::um::synchapi::{ CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, WaitForMultipleObjects };
use winapi::um::winnt::{ EVENT_MODIFY_STATE, HANDLE };

/// Session-local (`Local\`) so two different users logged into the same
/// machine each get their own instance, which is the behavior people expect
/// from a per-user tray app. A `Global\` name would let one user's running
/// copy block another's.
const MUTEX_PREFIX: &str = r"Local\MechvibesDX-SingleInstance-";

/// Event a refused second instance sets to raise the running window. Shares
/// the mutex's session-local scope for the same reason: one user's second
/// launch must never surface another user's window.
const WAKE_EVENT_PREFIX: &str = r"Local\MechvibesDX-ShowWindow-";

/// Event an elevated second instance sets to ask the lower-privileged
/// instance to gracefully terminate so the elevated instance can take over.
const QUIT_EVENT_PREFIX: &str = r"Local\MechvibesDX-Quit-";

/// `ERROR_ALREADY_EXISTS`. Defined here rather than pulled from winapi's
/// `winerror` module, which this crate does not enable as a feature; the
/// value is fixed by the Win32 ABI.
const ERROR_ALREADY_EXISTS: u32 = 183;

/// `WAIT_OBJECT_0` - the wait returned because the event was signalled.
/// Same rationale as `ERROR_ALREADY_EXISTS`: fixed by the Win32 ABI.
const WAIT_OBJECT_0: u32 = 0;

/// `WAIT_ABANDONED` - the mutex was abandoned by the terminating instance.
const WAIT_ABANDONED: u32 = 0x0000_0080;

/// `INFINITE` - wait without timeout. Defined here for the same reason as the
/// constants above: winapi's `winbase` feature is not enabled by this crate.
const INFINITE: u32 = 0xffff_ffff;

/// Create a SECURITY_ATTRIBUTES struct with a NULL DACL so that processes of
/// different integrity levels (e.g. Medium vs High/Admin) within the same user
/// session can open and signal mutual sync events.
unsafe fn create_permissive_security_attributes() -> (
    Option<winapi::um::minwinbase::SECURITY_ATTRIBUTES>,
    Option<Box<winapi::um::winnt::SECURITY_DESCRIPTOR>>,
) {
    unsafe {
        let mut sd = Box::new(std::mem::zeroed::<winapi::um::winnt::SECURITY_DESCRIPTOR>());
        if winapi::um::securitybaseapi::InitializeSecurityDescriptor(
            sd.as_mut() as *mut _ as *mut _,
            winapi::um::winnt::SECURITY_DESCRIPTOR_REVISION,
        ) == 0
        {
            return (None, None);
        }
        if winapi::um::securitybaseapi::SetSecurityDescriptorDacl(
            sd.as_mut() as *mut _ as *mut _,
            1,
            null_mut(),
            0,
        ) == 0
        {
            return (None, None);
        }
        let sa = winapi::um::minwinbase::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<winapi::um::minwinbase::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.as_mut() as *mut _ as *mut _,
            bInheritHandle: 0,
        };
        (Some(sa), Some(sd))
    }
}

/// Builds a lock name unique to this executable, so only *the same build*
/// blocks itself. Two copies of the installed app still refuse to co-run,
/// which is the point; but a `dx serve` dev build and an installed release
/// live at different paths and can run side by side, which is what you want
/// while developing. A single shared name makes `dx serve` fail to start
/// whenever the installed app happens to be running, with an "already
/// running" message that points at the wrong thing entirely.
///
/// The path is hashed rather than embedded: a mutex name has a length limit
/// and treats backslashes as kernel-namespace separators, both of which a
/// raw path would trip over.
///
/// Canonicalized first so that the same executable reached by different
/// spellings - an 8.3 short name, a `subst` drive, a symlink - still hashes
/// to one name. Lowercasing alone only settles case, and two spellings of one
/// exe would each claim their own mutex and defeat the guard entirely.
/// Canonicalization can fail (the file was moved out from under us); the
/// uncanonicalized path is a fine fallback, since a missed alias is no worse
/// than the behavior before.
fn mutex_name() -> String {
    format!("{}{:x}", MUTEX_PREFIX, exe_hash())
}

/// Hash identifying this executable, shared by the instance mutex and the
/// wake-up event so both name the same "copy of the app".
fn exe_hash() -> u64 {
    let exe = std::env::current_exe()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut hasher = DefaultHasher::new();
    exe.hash(&mut hasher);
    hasher.finish()
}

/// Name of the event a refused instance sets to surface the running window.
/// Derived from the same exe hash as the mutex, so a second copy only ever
/// wakes the instance that actually turned it away.
fn wake_event_name() -> String {
    format!("{}{:x}", WAKE_EVENT_PREFIX, exe_hash())
}

/// Name of the event an elevated replacing instance sets to ask the running
/// instance to exit.
fn quit_event_name() -> String {
    format!("{}{:x}", QUIT_EVENT_PREFIX, exe_hash())
}

/// Encode a name for the Win32 `*W` APIs.
fn wide(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
}

/// Ask the already-running instance to show and focus its window.
///
/// Called by the copy that lost the single-instance race, just before it
/// exits. Without this the second launch is completely silent - the binary is
/// built with `#![windows_subsystem = "windows"]`, so even the "already
/// running" message on stderr goes nowhere, and clicking the icon again looks
/// like nothing happened.
///
/// Best-effort by design: if the event cannot be opened (the other instance
/// exited in between, or it predates this feature) the second copy still
/// exits quietly, which is the old behavior rather than a new failure.
pub fn signal_running_instance() {
    let name = wide(&wake_event_name());

    // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
    // A null handle means "not found", which is handled rather than used.
    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if handle.is_null() {
            return;
        }
        SetEvent(handle);
        CloseHandle(handle);
    }
}

/// Signal the running instance to quit gracefully so this new (elevated) instance
/// can take over.
pub fn signal_quit_instance() {
    let name = wide(&quit_event_name());

    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if handle.is_null() {
            return;
        }
        SetEvent(handle);
        CloseHandle(handle);
    }
}

/// Watch for wake-up and quit events from other launches.
pub fn listen_for_instance_events(
    on_wake: impl Fn() + Send + 'static,
    on_quit: impl Fn() + Send + 'static,
) {
    let wake_name = wide(&wake_event_name());
    let quit_name = wide(&quit_event_name());

    let (sa, _sd) = unsafe { create_permissive_security_attributes() };
    let sa_ptr = sa.as_ref().map(|s| s as *const _ as *mut _).unwrap_or(null_mut());

    let wake_handle = unsafe { CreateEventW(sa_ptr, 0, 0, wake_name.as_ptr()) };
    let quit_handle = unsafe { CreateEventW(sa_ptr, 0, 0, quit_name.as_ptr()) };

    if wake_handle.is_null() || quit_handle.is_null() {
        if !wake_handle.is_null() {
            unsafe { CloseHandle(wake_handle); }
        }
        if !quit_handle.is_null() {
            unsafe { CloseHandle(quit_handle); }
        }
        crate::always_eprint!("⚠️ Could not create single-instance events");
        return;
    }

    let pair = EventPair(wake_handle, quit_handle);

    std::thread::spawn(move || {
        let events = pair.handles();
        loop {
            let result = unsafe {
                WaitForMultipleObjects(
                    2,
                    events.as_ptr(),
                    0,
                    INFINITE,
                )
            };

            if result == WAIT_OBJECT_0 {
                on_wake();
            } else if result == WAIT_OBJECT_0 + 1 {
                crate::always_print!("🛑 Received quit request from another instance; exiting");
                on_quit();
                break;
            } else {
                break;
            }
        }
    });
}

/// Watch for a second launch and run `on_wake` each time one happens.
/// If a quit request arrives, the process exits cleanly.
pub fn listen_for_wake_requests(on_wake: impl Fn() + Send + 'static) {
    listen_for_instance_events(on_wake, || {
        std::process::exit(0);
    });
}

/// Owns the wake-up and quit event handles so they can cross the thread boundary safely.
struct EventPair(HANDLE, HANDLE);

// SAFETY: the handles are only touched by the listener thread that takes
// ownership of them; Windows handles are process-wide, so the move is sound.
unsafe impl Send for EventPair {}

impl EventPair {
    fn handles(&self) -> [HANDLE; 2] {
        [self.0, self.1]
    }
}

impl Drop for EventPair {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                CloseHandle(self.0);
            }
            if !self.1.is_null() {
                CloseHandle(self.1);
            }
        }
    }
}

/// Holds the instance lock for as long as it is alive. Windows frees the
/// underlying mutex on process exit even if this is never dropped (a crash,
/// a `TerminateProcess`), so a stuck lock cannot outlive the process.
pub struct InstanceGuard(HANDLE);

// The handle is only closed on drop and never used concurrently; Windows
// handles are process-wide, so moving one across threads is fine.
unsafe impl Send for InstanceGuard {}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Claims the single-instance lock, returning `None` if another copy of the
/// app already holds it - in which case the caller should exit without
/// starting anything.
///
/// If this copy is running with elevated administrator privileges, it signals
/// the existing instance to exit and attempts to take over the mutex.
pub fn acquire() -> Option<InstanceGuard> {
    let name = wide(&mutex_name());

    let (sa, _sd) = unsafe { create_permissive_security_attributes() };
    let sa_ptr = sa.as_ref().map(|s| s as *const _ as *mut _).unwrap_or(null_mut());

    let handle = unsafe { CreateMutexW(sa_ptr, 0, name.as_ptr()) };

    if handle.is_null() {
        return Some(InstanceGuard(null_mut()));
    }

    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // If THIS instance is running as administrator, ask the existing instance to quit and take over
        if crate::utils::platform::is_elevated() {
            crate::always_print!("⚡ Elevated instance detected existing instance; signaling it to quit...");
            signal_quit_instance();

            // Wait up to 3000ms for the other instance to terminate and release the mutex
            let wait_res = unsafe { WaitForSingleObject(handle, 3000) };
            if wait_res == WAIT_OBJECT_0 || wait_res == WAIT_ABANDONED {
                crate::always_print!("✅ Successfully took over single-instance mutex as elevated process");
                return Some(InstanceGuard(handle));
            }
            crate::always_eprint!("⚠️ Timed out waiting for running instance to exit");
        }

        unsafe {
            CloseHandle(handle);
        }
        return None;
    }

    Some(InstanceGuard(handle))
}

