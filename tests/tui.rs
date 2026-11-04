use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthChar;
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Session {
    root: PathBuf,
    child: Child,
    master: File,
    slave: File,
    original: libc::termios,
    bytes: Vec<u8>,
    width: usize,
    height: usize,
}
impl Session {
    fn open(mode: &str, steps: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sj-tui-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("config/sj")).unwrap();
        fs::write(root.join("mode"), mode).unwrap();
        if mode == "catalog" {
            fs::write(root.join("slow-catalog"), "").unwrap();
        }
        fs::write(root.join("config/sj/config.toml"),"[tui]\nrefresh_seconds=1\n[views.queue]\norder=['JobId','Name','TimeLeft','tres-alloc']\nrules=[]\n[views.queue.columns.Name]\npreferred_width=20\n[views.queue.columns.tres-alloc]\npreferred_width=40\n[views.queue.columns.tres-alloc.entries]\ninclude=['cpu','mem','gres/*']\n").unwrap();
        let body = r#"
n=0; if [ -f "$SJ_TEST_ROOT/calls" ]; then n=$(cat "$SJ_TEST_ROOT/calls"); fi
n=$((n+1))
# Readers must see a complete old or new value, including during manual refresh.
: > "$SJ_TEST_ROOT/calls.next"
if [ -f "$SJ_TEST_ROOT/hold-count" ]; then
  : > "$SJ_TEST_ROOT/count-pending"
  while [ -f "$SJ_TEST_ROOT/hold-count" ] && [ ! -f "$SJ_TEST_ROOT/release-count" ]; do sleep 0.01; done
fi
printf '%s' "$n" > "$SJ_TEST_ROOT/calls.next"
mv "$SJ_TEST_ROOT/calls.next" "$SJ_TEST_ROOT/calls"
mode=$(cat "$SJ_TEST_ROOT/mode")
if [ "$mode" = slow ]; then sleep 30; fi
if [ "$mode" = fail ]; then printf 'controller temporarily unavailable\n' >&2; exit 1; fi
if [ "$mode" = empty ]; then exit 0; fi
i=0
while [ "$i" -lt 30 ]; do
  id=$((42+i))
  printf '%s\037task-%02d-long-训练-name\037demo\037R\0370:01:00\0371\037None\0372030-01-02T03:04:05\037compute\037node-01\0371:00:00\037cpu=8,mem=32G,node=1,billing=8,gres/gpu=2\n' "$id" "$i"
  i=$((i+1))
done
"#;
        let queue = format!(
            "if [ -f \"$SJ_TEST_ROOT/slow-catalog\" ]; then printf '%s' \"$$\" > \"$SJ_TEST_ROOT/catalog-pid\"; exec sleep 30; fi\n{}",
            include_str!("fixtures/squeue.sh").replace("__BODY__", body)
        );
        let controller = r#"
printf '%s\n' "$*" >> "$SJ_TEST_ROOT/controller-calls"
for id in "$@"; do :; done
if [ -f "$SJ_TEST_ROOT/slow-detail" ]; then sleep 30; fi
if [ -f "$SJ_TEST_ROOT/legacy" ] && [ "$1" = '--json' ]; then printf "unrecognized option '--json'\n" >&2; exit 1; fi
if [ "$1" != '--json' ]; then printf 'JobId=%s LegacyDetail=available\n' "$id"; exit 0; fi
case " $* " in *' show step '*) records=steps;; *) records=jobs;; esac
printf '{"%s":[{"job_id":"%s","name":"controller-detail-%s","tres_alloc_str":"cpu=8,billing=8","standard_output":"/work/job.out"}]}\n' "$records" "$id" "$id"
"#;
        for (name, body) in [("squeue", queue), ("scontrol", controller.into())] {
            let path = root.join("bin").join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (width, height) = (80, 24);
        let mut master = -1;
        let mut slave = -1;
        let size = libc::winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &size,
                )
            },
            0
        );
        for fd in [master, slave] {
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut original = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut original) },
            0
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_sj"));
        command.args(["q", "--tui", "--color", "never"]);
        if steps {
            command.arg("--steps");
        }
        command
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", root.join("bin").display()),
            )
            .env("HOME", &root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("SJ_TEST_ROOT", &root)
            .env("TERM", "xterm-256color")
            .env_remove("SQUEUE_STATES")
            .env_remove("SQUEUE_SORT")
            .env_remove("NO_COLOR")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = command.spawn().unwrap();
        Self {
            root,
            child,
            master,
            slave,
            original,
            bytes: Vec::new(),
            width: usize::from(width),
            height: usize::from(height),
        }
    }
    fn pump(&mut self) {
        let mut fd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, 30) } > 0 {
            let mut bytes = [0; 16384];
            if let Ok(count) = self.master.read(&mut bytes) {
                self.bytes.extend_from_slice(&bytes[..count]);
            }
        }
    }
    fn screen(&self) -> String {
        screen(&self.bytes, self.width, self.height)
    }
    fn until(&mut self, text: &str) {
        self.until_timeout(text, Duration::from_secs(8));
    }
    fn until_timeout(&mut self, text: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !self.screen().contains(text) && Instant::now() < deadline {
            self.pump();
        }
        assert!(
            self.screen().contains(text),
            "missing {text:?}:\n{}\nraw: {:?}",
            self.screen(),
            String::from_utf8_lossy(&self.bytes)
        );
    }
    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }
    fn count(&self) -> usize {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .parse()
            .unwrap_or(0)
    }
    fn resize(&mut self, width: u16, height: u16) {
        let size = libc::winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) },
            0
        );
        self.width = usize::from(width);
        self.height = usize::from(height);
    }
    fn finish(&mut self, code: i32) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            self.pump();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(code));
                break;
            }
            assert!(Instant::now() < deadline, "TUI did not exit");
        }
        self.pump();
        let text = String::from_utf8_lossy(&self.bytes);
        assert!(text.contains("\x1b[?1049h"));
        assert!(text.contains("\x1b[?1049l"));
        assert!(text.contains("\x1b[?25h"));
        let mut actual = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut actual) },
            0
        );
        assert_eq!(actual.c_iflag, self.original.c_iflag);
        assert_eq!(actual.c_oflag, self.original.c_oflag);
        assert_eq!(actual.c_lflag, self.original.c_lflag);
        assert_eq!(actual.c_cc[libc::VMIN], self.original.c_cc[libc::VMIN]);
        assert_eq!(actual.c_cc[libc::VTIME], self.original.c_cc[libc::VTIME]);
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

// Minimal screen decoder for the cursor moves and clears emitted by Crossterm.
fn screen(bytes: &[u8], width: usize, height: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut chars = text.chars().peekable();
    let mut grid = vec![vec![' '; width]; height];
    let (mut x, mut y) = (0usize, 0usize);
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.next() != Some('[') {
                continue;
            }
            let mut body = String::new();
            let mut final_char = ' ';
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    final_char = next;
                    break;
                }
                body.push(next);
            }
            let values: Vec<usize> = body.split(';').map(|s| s.parse().unwrap_or(0)).collect();
            let n = values.first().copied().unwrap_or(0).max(1);
            match final_char {
                'H' | 'f' => {
                    y = n - 1;
                    x = values.get(1).copied().unwrap_or(1).max(1) - 1;
                }
                'G' => x = n - 1,
                'A' => y = y.saturating_sub(n),
                'B' => y = y.saturating_add(n),
                'C' => x = x.saturating_add(n),
                'D' => x = x.saturating_sub(n),
                'J' if body == "2" || body == "3" => grid = vec![vec![' '; width]; height],
                'K' => {
                    if y < height {
                        grid[y][x.min(width)..width].fill(' ');
                    }
                }
                'h' if body == "?1049" => {
                    grid = vec![vec![' '; width]; height];
                    x = 0;
                    y = 0;
                }
                _ => (),
            }
        } else if c == '\r' {
            x = 0;
        } else if c == '\n' {
            y = (y + 1).min(height.saturating_sub(1));
        } else if !c.is_control() {
            let size = c.width().unwrap_or(0);
            if size == 0 {
                continue;
            }
            if x >= width {
                x = 0;
                y = y.saturating_add(1);
            }
            if y < height && x < width {
                grid[y][x] = c;
                grid[y][x + 1..(x + size).min(width)].fill(' ');
            }
            x = x.saturating_add(size);
        }
    }
    grid.iter()
        .map(|row| row.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn arrows_pages_horizontal_scroll_details_and_resize_restore_terminal() {
    let mut session = Session::open("ok", false);
    session.until("LIVE");
    assert!(!session.root.join("controller-calls").exists());
    session.send(b"\x1b[B\r");
    session.until("Task 43");
    session.until("Controller:");
    session.send(b"\t");
    session.until("QUEUE FIELDS");
    session.send(b"\x1b[F");
    session.until("billing=8");
    session.send(b"q\x1b[6~\r");
    session.until("Task 61");
    session.send(b"q\x1b[F");
    session.until("Row 30/30");
    session.resize(50, 12);
    session.send(b"\x1b[1;5C");
    session.until("Columns 2");
    session.send(b"?");
    session.until("Queue shortcuts");
    session.send(b"q");
    session.resize(8, 3);
    session.until("Resize");
    session.send(b"q");
    session.finish(0);
}

#[test]
fn pause_rate_changes_and_manual_refresh_do_not_edit_config() {
    let mut session = Session::open("ok", false);
    session.until("LIVE");
    let config = fs::read(session.root.join("config/sj/config.toml")).unwrap();
    session.send(b" --");
    session.until("PAUSED");
    session.until("Every 3s");
    let count = session.count();
    assert!(count > 0);
    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        session.pump();
    }
    assert_eq!(session.count(), count);
    fs::write(session.root.join("hold-count"), "").unwrap();
    session.send(b"r");
    let pending = session.root.join("count-pending");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !pending.exists() && Instant::now() < deadline {
        session.pump();
    }
    assert!(
        pending.exists(),
        "manual query did not reach the counter update"
    );
    assert_eq!(
        session.count(),
        count,
        "incomplete update changed the count"
    );
    fs::write(session.root.join("release-count"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while session.count() == count && Instant::now() < deadline {
        session.pump();
    }
    assert_eq!(session.count(), count + 1);
    assert_eq!(
        fs::read(session.root.join("config/sj/config.toml")).unwrap(),
        config
    );
    session.send(b"q");
    session.finish(0);
}

#[test]
fn slow_queries_do_not_block_keys_or_interrupt_cleanup() {
    let mut session = Session::open("slow", false);
    session.until("Loading queue");
    session.send(b"?");
    session.until("Queue shortcuts");
    let start = Instant::now();
    session.send(&[3]);
    session.finish(130);
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn errors_preserve_rows_and_signal_exit_restores_terminal() {
    let mut session = Session::open("ok", false);
    session.until("LIVE");
    fs::write(session.root.join("mode"), "fail").unwrap();
    session.send(b"r");
    session.until("STALE");
    assert!(session.screen().contains("task-00"));
    fs::write(session.root.join("mode"), "ok").unwrap();
    session.send(b"r");
    session.until("LIVE");
    unsafe {
        libc::kill(session.child.id() as i32, libc::SIGTERM);
    }
    session.finish(143);
}

#[test]
fn step_details_and_legacy_text_fallback_remain_available() {
    let mut session = Session::open("ok", true);
    session.until("LIVE");
    fs::write(session.root.join("legacy"), "").unwrap();
    session.send(b"\r");
    session.until("Task 42.0");
    session.until("Controller:");
    session.send(b"\t\x1b[F");
    session.until("LegacyDetail=available");
    let calls = fs::read_to_string(session.root.join("controller-calls")).unwrap();
    assert!(calls.contains("--json show step 42.0"));
    assert!(calls.contains("--details show step 42.0"));
    session.send(b"qq");
    session.finish(0);
}

#[test]
fn query_timeout_can_be_retried_without_leaving_the_screen() {
    let mut session = Session::open("slow", false);
    session.until("Loading queue");
    session.until_timeout("timed out after 10s", Duration::from_secs(13));
    fs::write(session.root.join("mode"), "ok").unwrap();
    session.send(b"r");
    session.until("LIVE");
    session.send(b"q");
    session.finish(0);
}

#[test]
fn closing_a_slow_detail_cannot_overwrite_the_next_task_popup() {
    let mut session = Session::open("ok", false);
    session.until("LIVE");
    fs::write(session.root.join("slow-detail"), "").unwrap();
    session.send(b"\r");
    session.until("Task 42");
    session.until("Loading controller");
    session.send(b"q");
    fs::remove_file(session.root.join("slow-detail")).unwrap();
    session.send(b"\x1b[B\r");
    session.until("Task 43");
    session.until("Controller:");
    assert!(session.screen().contains("controller-detail-43"));
    assert!(!session.screen().contains("controller-detail-42"));
    fs::write(session.root.join("mode"), "empty").unwrap();
    session.until("Task is not in the latest queue snapshot");
    assert!(session.screen().contains("Task 43"));
    session.send(b"qq");
    session.finish(0);
}

#[test]
fn interrupted_startup_catalogs_reap_children_before_the_screen_opens() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let mut session = Session::open("catalog", false);
        let pidfile = session.root.join("catalog-pid");
        let deadline = Instant::now() + Duration::from_secs(4);
        while !pidfile.exists() && Instant::now() < deadline {
            session.pump();
        }
        let pid: i32 = fs::read_to_string(pidfile).unwrap().parse().unwrap();
        if signal == libc::SIGINT {
            session.send(&[3]);
        } else {
            unsafe {
                libc::kill(session.child.id() as i32, signal);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            session.pump();
            if let Some(status) = session.child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(128 + signal));
                break;
            }
            assert!(Instant::now() < deadline, "startup did not cancel");
        }
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "catalog child was not reaped"
        );
        assert!(!session.bytes.windows(8).any(|s| s == b"\x1b[?1049h"));
        let mut actual = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(session.slave.as_raw_fd(), &mut actual) },
            0
        );
        assert_eq!(actual.c_iflag, session.original.c_iflag);
        assert_eq!(actual.c_lflag, session.original.c_lflag);
    }
}
