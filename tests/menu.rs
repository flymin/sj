use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Menu {
    root: PathBuf,
    child: Child,
    master: File,
    slave: File,
    original: libc::termios,
    output: Vec<u8>,
}

impl Menu {
    fn open(height: u16, width: u16) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sj-pty-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        let uid = unsafe { libc::geteuid() };
        let controller = format!(
            "#!/bin/sh\nif [ \"$1\" = '--json' ]; then\n  printf '%s\\n' '{{\"jobs\":[{{\"job_id\":42,\"user_id\":{uid},\"job_state\":[\"RUNNING\"],\"nodes\":\"test-nodes\"}}]}}'\nelse\n  i=0; while [ \"$i\" -lt 32 ]; do printf 'node-%s\\n' \"$i\"; i=$((i+1)); done\nfi\n"
        );
        for (name, body) in [
            ("scontrol", controller),
            (
                "srun",
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"\nprintf 'STEP_OK\\n'\n"
                    .into(),
            ),
        ] {
            let path = root.join("bin").join(name);
            fs::write(&path, body).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut master = -1;
        let mut slave = -1;
        let ws = libc::winsize {
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
                    &ws,
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
        command
            .args(["sh", "42"])
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", root.join("bin").display()),
            )
            .env("TERM", "xterm-256color")
            .env("SJ_TEST_CALLS", root.join("calls"))
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let mut menu = Self {
            root,
            child,
            master,
            slave,
            original,
            output: Vec::new(),
        };
        menu.until("↑↓ move");
        menu
    }

    fn pump(&mut self) {
        let mut fd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, 50) } > 0 {
            let mut buf = [0; 8192];
            if let Ok(n) = self.master.read(&mut buf) {
                self.output.extend_from_slice(&buf[..n]);
            }
        }
    }

    fn until(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !String::from_utf8_lossy(&self.output).contains(needle) && Instant::now() < deadline {
            self.pump();
        }
        assert!(
            String::from_utf8_lossy(&self.output).contains(needle),
            "missing {needle:?}: {}",
            String::from_utf8_lossy(&self.output)
        );
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    fn resize(&self, height: u16, width: u16) {
        let ws = libc::winsize {
            ws_row: height,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) },
            0
        );
    }

    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.pump();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "menu did not exit");
        }
        self.pump();
        let output = String::from_utf8_lossy(&self.output);
        assert!(output.contains("\x1b[?1049l"));
        assert!(output.contains("\x1b[?25h"));
        let mut restored = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut restored) },
            0
        );
        assert_eq!(restored.c_iflag, self.original.c_iflag);
        assert_eq!(restored.c_oflag, self.original.c_oflag);
        assert_eq!(restored.c_lflag, self.original.c_lflag);
        assert_eq!(restored.c_cc[libc::VMIN], self.original.c_cc[libc::VMIN]);
        assert_eq!(restored.c_cc[libc::VTIME], self.original.c_cc[libc::VTIME]);
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn cursor_rows(output: &[u8]) -> Vec<usize> {
    String::from_utf8_lossy(output)
        .split("\x1b[")
        .filter_map(|s| {
            let (number, tail) = s.split_once(';')?;
            if tail.starts_with("1H") {
                number.parse().ok()
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn enter_arrows_and_numeric_entry_select_the_expected_node() {
    for (keys, expected) in [
        (b"\r".as_slice(), 0),
        (b"\x1b[B\x1b[B\x1b[A\r".as_slice(), 1),
        (b"12\r".as_slice(), 12),
    ] {
        let mut menu = Menu::open(8, 50);
        assert!(cursor_rows(&menu.output).iter().all(|row| *row <= 8));
        menu.send(keys);
        menu.finish();
        let args = fs::read_to_string(menu.root.join("calls")).unwrap();
        assert!(
            args.lines()
                .any(|line| line == format!("--nodelist=node-{expected}"))
        );
    }
}

#[test]
fn invalid_number_waits_and_backspace_corrects_it() {
    let mut menu = Menu::open(8, 80);
    menu.send(b"99\r");
    menu.until("invalid index: 99");
    assert!(!menu.root.join("calls").exists());
    menu.send(b"\x7f\r");
    menu.finish();
    assert!(
        fs::read_to_string(menu.root.join("calls"))
            .unwrap()
            .contains("--nodelist=node-9\n")
    );
}

#[test]
fn resize_scrolls_without_exceeding_height_and_escape_restores_tty() {
    let mut menu = Menu::open(10, 80);
    menu.send(b"\x1b[F");
    menu.until("selected 31");
    let start = menu.output.len();
    menu.resize(6, 32);
    menu.until("31-32/32");
    let rows = cursor_rows(&menu.output[start..]);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| *row <= 6));
    menu.send(b"\x1b");
    menu.finish();
    assert!(!menu.root.join("calls").exists());
}

#[test]
fn termination_signal_restores_terminal_settings() {
    let mut menu = Menu::open(8, 80);
    assert_eq!(
        unsafe { libc::kill(menu.child.id() as i32, libc::SIGTERM) },
        0
    );
    menu.finish();
    assert!(!menu.root.join("calls").exists());
}
