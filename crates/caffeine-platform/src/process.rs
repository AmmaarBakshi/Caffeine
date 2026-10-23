//! Process helpers: detaching session hosts and stopping them forcibly.

use std::process::Command;

use caffeine_core::{Error, Result};

/// Configure `cmd` so the spawned process outlives the invoking terminal:
/// a new session on Unix (no controlling terminal, immune to the terminal's
/// SIGHUP and Ctrl+C), and a detached, console-less process in its own
/// process group on Windows.
pub fn configure_detached(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows_flags::DETACHED_PROCESS | windows_flags::CREATE_NEW_PROCESS_GROUP);
    }
}

/// Like [`configure_detached`], and additionally ask Windows to let the
/// process break away from the parent's job object (terminals such as IDE
/// consoles kill their job when they close). Callers should retry with
/// [`configure_detached`] alone if spawning fails with this flag, because
/// breakaway is not permitted by every job.
#[cfg(windows)]
pub fn configure_detached_breakaway(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(
        windows_flags::DETACHED_PROCESS
            | windows_flags::CREATE_NEW_PROCESS_GROUP
            | windows_flags::CREATE_BREAKAWAY_FROM_JOB,
    );
}

#[cfg(windows)]
mod windows_flags {
    pub const DETACHED_PROCESS: u32 = 0x0000_0008;
    pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
}

/// Whether a process with this PID currently exists.
pub fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 performs only the existence/permission check.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Forcibly terminate a process. Used only by `caffeine stop --force` when a
/// session host does not respond. Process-bound power assertions are
/// released by the operating system when the process dies.
pub fn kill(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let raw = libc::pid_t::try_from(pid)
            .map_err(|_| Error::usage(format!("invalid pid {pid}")))?;
        // SAFETY: sending a signal has no memory-safety implications.
        if unsafe { libc::kill(raw, libc::SIGKILL) } != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(Error::io(format!("killing process {pid}"), err));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                if !is_alive(pid) {
                    return Ok(());
                }
                let code = GetLastError();
                return Err(Error::permission(format!("cannot open process {pid} to terminate it"))
                    .with_os_code(i64::from(code)));
            }
            let ok = TerminateProcess(h, 1) != 0;
            let code = GetLastError();
            CloseHandle(h);
            if ok {
                Ok(())
            } else {
                Err(Error::failure(format!("TerminateProcess({pid}) failed"))
                    .with_os_code(i64::from(code)))
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(Error::unsupported(format!("cannot terminate process {pid} on this platform")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn kill_terminates_a_child() {
        #[cfg(windows)]
        let mut child = Command::new("cmd").args(["/C", "ping -n 30 127.0.0.1 >NUL"]).spawn().unwrap();
        #[cfg(unix)]
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        assert!(is_alive(pid));
        kill(pid).unwrap();
        child.wait().unwrap();
        assert!(!is_alive(pid));
    }
}
