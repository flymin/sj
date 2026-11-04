use crate::util::{self, Result};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(10);
pub static INTERRUPTED: AtomicI32 = AtomicI32::new(0);
pub const WORKER_STOPPED: &str = "query worker stopped";

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        // Slurm wrappers may spawn children that also hold the capture pipes.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(format!(
            "configure query pipe: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}
fn drain<T: Read>(pipe: &mut Option<T>, bytes: &mut Vec<u8>, finished: bool) -> Result<()> {
    let Some(stream) = pipe.as_mut() else {
        return Ok(());
    };
    // Bound each turn so continuous output cannot starve cancellation/deadlines.
    for _ in 0..8 {
        let mut buffer = [0; 16384];
        match stream.read(&mut buffer) {
            Ok(0) => {
                *pipe = None;
                return Ok(());
            }
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Once the command exits, do not wait for detached pipe owners.
                if finished {
                    *pipe = None;
                }
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(format!("read query pipe: {e}")),
        }
    }
    Ok(())
}
fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed) || INTERRUPTED.load(Ordering::Relaxed) != 0
}
pub fn capture(command: &mut Command, cancel: &AtomicBool, timeout: Duration) -> Result<String> {
    if cancelled(cancel) {
        return Err("query cancelled".into());
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("query timeout too large")?;
    let name = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = Process(
        command
            .spawn()
            .map_err(|e| format!("cannot run {name}: {e}"))?,
    );
    let stdout = child.0.stdout.take().ok_or("missing query stdout")?;
    let stderr = child.0.stderr.take().ok_or("missing query stderr")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut stdout = Some(stdout);
    let mut stderr = Some(stderr);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut status = None;
    let status = loop {
        if cancelled(cancel) {
            return Err("query cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err(format!("{name} timed out after {}s", timeout.as_secs()));
        }
        if status.is_none() {
            status = child
                .0
                .try_wait()
                .map_err(|e| format!("wait for {name}: {e}"))?;
            if status.is_some() {
                unsafe {
                    libc::kill(-(child.0.id() as i32), libc::SIGKILL);
                }
            }
        }
        drain(&mut stdout, &mut out, status.is_some())?;
        drain(&mut stderr, &mut err, status.is_some())?;
        if let Some(status) = status
            && stdout.is_none()
            && stderr.is_none()
        {
            break status;
        }
        let mut pipes = [
            libc::pollfd {
                fd: stdout.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stderr.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(20) as i32;
        if unsafe { libc::poll(pipes.as_mut_ptr(), pipes.len() as libc::nfds_t, remaining) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(format!("poll query pipes: {error}"));
            }
        }
    };
    if !status.success() {
        return Err(format!(
            "{name} exited with {}: {}",
            util::exit_code(status),
            String::from_utf8_lossy(&err).trim()
        ));
    }
    String::from_utf8(out).map_err(|e| format!("invalid UTF-8 from {name}: {e}"))
}

pub struct Task<T> {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    receiver: mpsc::Receiver<Result<T>>,
}
impl<T: Send + 'static> Task<T> {
    pub fn spawn(work: impl FnOnce(&AtomicBool) -> Result<T> + Send + 'static) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = sender.send(work(&token));
        });
        Self {
            cancel,
            receiver,
            handle: Some(handle),
        }
    }
    pub fn poll(&self) -> Option<Result<T>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(WORKER_STOPPED.into())),
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaped_pipe_owners_do_not_defeat_deadlines_or_cancellation() {
        use std::os::unix::fs::DirBuilderExt;
        let directory =
            std::env::temp_dir().join(format!("sj-escaped-pipe-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let path = directory.join("owner.pid");
        for script in [
            "setsid sh -c 'echo $$ > \"$1\"; sleep 3' _ \"$1\" & wait",
            "setsid sh -c 'echo $$ > \"$1\"; sleep 3' _ \"$1\" & printf ready",
        ] {
            let start = Instant::now();
            let result = capture(
                Command::new("sh").args(["-c", script, "_", path.to_str().unwrap()]),
                &AtomicBool::new(false),
                Duration::from_millis(80),
            );
            let elapsed = start.elapsed();
            let deadline = Instant::now() + Duration::from_millis(300);
            while !path.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if let Ok(pid) = std::fs::read_to_string(&path)
                && let Ok(pid) = pid.trim().parse::<i32>()
            {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let _ = std::fs::remove_file(&path);
            assert!(elapsed < Duration::from_secs(2));
            if script.ends_with("wait") {
                assert!(result.unwrap_err().contains("timed out"));
            } else {
                assert_eq!(result.unwrap(), "ready");
            }
        }
        let task_path = path.clone();
        let task = Task::spawn(move |cancel| {
            capture(
                Command::new("sh").args([
                    "-c",
                    "setsid sh -c 'echo $$ > \"$1\"; sleep 3' _ \"$1\" & wait",
                    "_",
                    task_path.to_str().unwrap(),
                ]),
                cancel,
                TIMEOUT,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let start = Instant::now();
        drop(task);
        let elapsed = start.elapsed();
        if let Ok(pid) = std::fs::read_to_string(&path)
            && let Ok(pid) = pid.trim().parse::<i32>()
        {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(directory);
        assert!(elapsed < Duration::from_secs(2));
    }
    #[test]
    fn timeout_and_cancellation_reap_wrappers_without_waiting_for_descendants() {
        let start = Instant::now();
        let error = capture(
            Command::new("sh").args(["-c", "sleep 30 & wait"]),
            &AtomicBool::new(false),
            Duration::from_millis(80),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
        let task = Task::spawn(|cancel| {
            capture(
                Command::new("sh").args(["-c", "sleep 30 & wait"]),
                cancel,
                TIMEOUT,
            )
        });
        thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        drop(task);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn success_handles_large_pipes_and_background_children() {
        let text = capture(
            Command::new("sh").args(["-c", "head -c 100000 /dev/zero | tr '\\000' a; sleep 30 &"]),
            &AtomicBool::new(false),
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(text.len(), 100000);
    }
}
