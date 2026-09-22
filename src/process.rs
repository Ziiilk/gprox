use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub fn detach_standard_handles() -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation},
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    for kind in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // Only the inheritance flag changes; the launcher's streams stay usable.
        unsafe {
            let handle = GetStdHandle(kind);
            if !handle.is_null()
                && handle != INVALID_HANDLE_VALUE
                && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("Cannot detach launcher output handles");
            }
        }
    }
    Ok(())
}

pub fn resolve_codex(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            bail!("Codex executable not found: {}", path.display());
        }
        #[cfg(windows)]
        if path.extension().and_then(|s| s.to_str()) != Some("exe") {
            bail!("On Windows --codex must point to codex.exe, not a .cmd/.ps1 wrapper");
        }
        return Ok(path.to_owned());
    }
    let path = which::which("codex")
        .context("Codex not found. Install Codex CLI and run codex login, or use --codex")?;
    #[cfg(windows)]
    {
        if path.extension().and_then(|s| s.to_str()) == Some("exe") {
            return Ok(path);
        }
        // Resolve npm's wrapper to the native binary, without invoking cmd.exe.
        if let Some(parent) = path.parent()
            && let Some(found) = find_native(&parent.join("node_modules/@openai/codex"), 8)
        {
            return Ok(found);
        }
        bail!(
            "Cannot resolve native codex.exe from {}. Pass --codex <path-to-codex.exe>",
            path.display()
        );
    }
    #[cfg(not(windows))]
    Ok(path)
}

#[cfg(windows)]
fn find_native(root: &Path, depth: u8) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if entry.file_type().ok()?.is_dir() {
            if let Some(found) = find_native(&path, depth - 1) {
                return Some(found);
            }
        } else if entry.file_name() == "codex.exe" {
            return Some(path);
        }
    }
    None
}

/// Kill the backend's complete process tree when its request is dropped.
pub struct TreeGuard {
    #[cfg(windows)]
    handle: isize,
    #[cfg(unix)]
    pid: u32,
}

impl TreeGuard {
    pub fn attach(child: &tokio::process::Child) -> Result<Self> {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            );
            let process = child.raw_handle().context("Codex process already exited")?;
            if configured == 0 || AssignProcessToJobObject(job, process as _) == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(error).context("Cannot attach Codex process to cleanup job");
            }
            Ok(Self {
                handle: job as isize,
            })
        }
        #[cfg(unix)]
        {
            Ok(Self {
                pid: child.id().context("Codex process already exited")?,
            })
        }
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle as _);
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
    }
}
