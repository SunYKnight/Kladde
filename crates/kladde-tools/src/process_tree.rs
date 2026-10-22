//! A started process together with everything it starts, so "Stop" also ends grandchildren.

use std::process::{Child, Command};

use crate::ToolError;

/// Windows: a job object. `taskkill /T` is not enough because MSYS (Git Bash) children are created by
/// short-lived fork helpers, which breaks the parent-pid chain it follows.
#[cfg(windows)]
pub(crate) struct ProcessTree {
    /// Job handle stored as an integer so the owner stays `Send`.
    job: isize,
}

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        pub fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        pub fn TerminateJobObject(job: *mut c_void, exit_code: u32) -> i32;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
    }
}

#[cfg(windows)]
impl ProcessTree {
    pub(crate) fn prepare(_cmd: &mut Command) {}

    /// Puts a freshly spawned child into a new job; descendants inherit the membership.
    pub(crate) fn attach(child: &Child) -> Result<ProcessTree, ToolError> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: null attributes and name are valid (unnamed job).
        let job = unsafe { ffi::CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let tree =
            if job.is_null() { Err(std::io::Error::last_os_error()) } else { Ok(ProcessTree { job: job as isize }) }?;
        // SAFETY: both handles are valid while `child` is borrowed.
        let assigned = unsafe { ffi::AssignProcessToJobObject(job, child.as_raw_handle()) };
        if assigned == 0 {
            Err(ToolError::Io(std::io::Error::last_os_error()))
        } else {
            Ok(tree)
        }
    }

    pub(crate) fn kill(&self) -> Result<(), ToolError> {
        // SAFETY: `self.job` is a job handle owned by this value until drop.
        let ok = unsafe { ffi::TerminateJobObject(self.job as *mut std::ffi::c_void, 1) };
        if ok == 0 {
            Err(ToolError::Io(std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed exactly once.
        unsafe { ffi::CloseHandle(self.job as *mut std::ffi::c_void) };
    }
}

/// Unix: the child leads its own process group, and killing the group reaches its descendants.
#[cfg(unix)]
pub(crate) struct ProcessTree {
    group: u32,
}

#[cfg(unix)]
impl ProcessTree {
    pub(crate) fn prepare(cmd: &mut Command) {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    pub(crate) fn attach(child: &Child) -> Result<ProcessTree, ToolError> {
        Ok(ProcessTree { group: child.id() })
    }

    pub(crate) fn kill(&self) -> Result<(), ToolError> {
        let mut cmd = Command::new("kill");
        cmd.args(["-KILL", "--", &format!("-{}", self.group)]);
        crate::run_output(&mut cmd).map(drop)
    }
}
