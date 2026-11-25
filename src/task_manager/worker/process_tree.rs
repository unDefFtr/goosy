use anyhow::{Context, Result};
use std::io;
use std::process::{Child, Command, ExitStatus};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) struct ProcessTree {
    pub(super) child: Child,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

#[cfg(unix)]
impl ProcessTree {
    pub(super) fn spawn(command: &mut Command) -> Result<Self> {
        use std::os::unix::process::CommandExt;
        let child = command.process_group(0).spawn()?;
        Ok(Self { child })
    }

    pub(super) fn leader_exited(&mut self) -> Result<bool> {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        // Keep the leader waitable until we have signalled its group. Reaping it
        // here would let the kernel reuse its PID for an unrelated process group.
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        let id = Pid::from_child(&self.child);
        Ok(rustix::io::retry_on_intr(|| waitid(WaitId::Pid(id), options))?.is_some())
    }

    fn terminate(&mut self) -> Result<()> {
        use rustix::process::{Pid, Signal, kill_process_group};
        match kill_process_group(Pid::from_child(&self.child), Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(windows)]
mod windows_tree {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };
    use windows::core::PCWSTR;

    impl ProcessTree {
        pub(in super::super) fn spawn(command: &mut Command) -> Result<Self> {
            // Leave KILL_ON_JOB_CLOSE unset: if the manager crashes, stdin EOF
            // still lets the Worker commit its current segment before exiting.
            let job = unsafe {
                OwnedHandle::from_raw_handle(CreateJobObjectW(None, PCWSTR::null())?.0)
            };
            let child = command
                .creation_flags((CREATE_NO_WINDOW | CREATE_SUSPENDED).0)
                .spawn()?;
            let mut tree = Self { child, job: None };
            let setup = (|| -> Result<()> {
                unsafe {
                    AssignProcessToJobObject(
                        HANDLE(job.as_raw_handle()),
                        HANDLE(tree.child.as_raw_handle()),
                    )?;
                }
                tree.job = Some(job);
                resume_primary_thread(tree.child.id())
            })();
            if let Err(error) = setup {
                // Even an assignment failure owns a real (still suspended) child.
                if let Err(cleanup) = tree.reap([]) {
                    return Err(error.context(format!("清理暂停的 Worker：{cleanup:#}")));
                }
                return Err(error);
            }
            Ok(tree)
        }

        pub(in super::super) fn leader_exited(&mut self) -> Result<bool> {
            Ok(self.child.try_wait()?.is_some())
        }

        pub(super) fn terminate(&mut self) -> Result<()> {
            if let Some(job) = &self.job {
                unsafe { TerminateJobObject(HANDLE(job.as_raw_handle()), 1)? };
            } else {
                self.child.kill()?;
            }
            Ok(())
        }
    }

    fn resume_primary_thread(pid: u32) -> Result<()> {
        // std::process::Child exposes only the process handle. Before the
        // initial resume the Worker has exactly one thread and no descendants.
        let snapshot = unsafe {
            OwnedHandle::from_raw_handle(CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)?.0)
        };
        let handle = HANDLE(snapshot.as_raw_handle());
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        unsafe { Thread32First(handle, &mut entry)? };
        let mut primary = None;
        loop {
            if entry.th32OwnerProcessID == pid {
                anyhow::ensure!(primary.is_none(), "暂停的 Worker 有多个线程");
                primary = Some(entry.th32ThreadID);
            }
            match unsafe { Thread32Next(handle, &mut entry) } {
                Ok(()) => (),
                Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
                Err(error) => return Err(error.into()),
            }
        }
        let id = primary.context("找不到暂停的 Worker 主线程")?;
        let primary = unsafe {
            OwnedHandle::from_raw_handle(OpenThread(THREAD_SUSPEND_RESUME, false, id)?.0)
        };
        let count = unsafe { ResumeThread(HANDLE(primary.as_raw_handle())) };
        if count == u32::MAX {
            return Err(io::Error::last_os_error().into());
        }
        anyhow::ensure!(count == 1, "Worker 主线程暂停计数异常：{count}");
        Ok(())
    }
}

impl ProcessTree {
    /// Signal the whole tree before draining its inherited pipes. Only reap
    /// the leader once that signal succeeded (the Unix PGID is then safe).
    pub(super) fn reap<const N: usize>(
        mut self,
        readers: [JoinHandle<io::Result<()>>; N],
    ) -> Result<ExitStatus> {
        let start = Instant::now();
        let termination = self.terminate();
        let retry_kill = termination.is_err();
        let status = termination.and_then(|()| loop {
            if readers.iter().all(JoinHandle::is_finished)
                && let Some(status) = self.child.try_wait()?
            {
                break Ok(status);
            }
            anyhow::ensure!(start.elapsed() < Duration::from_secs(2), "等待 Worker 进程树回收超时");
            thread::sleep(Duration::from_millis(10));
        });
        match status {
            Ok(status) => {
                for reader in readers {
                    reader.join().map_err(|_| anyhow::anyhow!("Worker 管道读取线程异常退出"))??;
                }
                Ok(status)
            }
            Err(error) => {
                // The manager must remain stoppable even if the OS cannot kill
                // the tree promptly or a descendant still has a pipe open.
                thread::spawn(move || {
                    if retry_kill {
                        while self.terminate().is_err() {
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                    let _ = self.child.wait();
                    for reader in readers {
                        let _ = reader.join();
                    }
                });
                Err(error)
            }
        }
    }
}
