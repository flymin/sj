use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sj-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        Self { root }
    }
    fn script(&self, name: &str, body: &str) {
        let file = self.root.join("bin").join(name);
        let body = if name == "squeue" {
            include_str!("fixtures/squeue.sh").replace("__BODY__", body)
        } else {
            body.to_owned()
        };
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn output_script(&self, name: &str, text: &str) {
        self.script(name, &format!("printf '%s\\n' {}", quote(text)));
    }
    fn queue(&self, rows: &[String]) {
        self.script(
            "squeue",
            &format!(
                "printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"\nprintf '%s\\n' {}",
                quote(&rows.join("\n"))
            ),
        );
    }
    fn fail_catalog(&self, message: &str) {
        let path = self.root.join("bin/squeue");
        let body = fs::read_to_string(&path).unwrap();
        fs::write(path, format!("#!/bin/sh\nif {{ [ \"$#\" -eq 1 ] && [ \"$1\" = '--helpFormat' ]; }} || {{ [ \"$#\" -eq 2 ] && [ \"$1\" = '--steps' ] && [ \"$2\" = '--helpFormat' ]; }}; then printf '%s\\n' {} >&2; exit 1; fi\n{}", quote(message), body.strip_prefix("#!/bin/sh\n").unwrap())).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sj"))
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("TERM", "dumb")
            .env_remove("SQUEUE_STATES")
            .env_remove("SQUEUE_SORT")
            .env("SJ_TEST_CALLS", self.root.join("calls"))
            .env("COLUMNS", "80")
            .output()
            .unwrap()
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls")).unwrap()
    }
    fn job(&self, state: &str) {
        let uid = unsafe { libc::geteuid() };
        let json = format!(
            r#"{{"errors":[],"jobs":[{{"job_id":42,"user_id":{uid},"job_state":["{state}"],"nodes":"node-c,node-a,node-b","standard_output":"logs/a b-%06j.out","standard_error":"","current_working_directory":"/shared work"}}]}}"#
        );
        self.script("scontrol", &format!("if [ \"$1\" = '--json' ]; then printf '%s\\n' {}; else printf '%s\\n' node-c node-a node-b; fi", quote(&json)));
    }
    fn submitted_script(&self, raw: u32, script: &[u8]) {
        fs::write(self.root.join("submitted.sbatch"), script).unwrap();
        let json = format!(r#"{{"jobs":[{{"job_id":{raw}}}]}}"#);
        self.script("scontrol", &format!(
            "if [ \"$1\" = '--json' ]; then printf '%s\\n' {}; else printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"; cat {}; fi",
            quote(&json), quote(self.root.join("submitted.sbatch").to_str().unwrap())
        ));
        self.script(
            "sacct",
            "printf 'unexpected accounting query\\n' >&2; exit 91",
        );
        self.script("less", "printf 'unexpected pager\\n' >&2; exit 92");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn adaptive_queue_forwards_filters_and_keeps_complete_id() {
    let f = Fixture::new();
    let row = [
        "123456789_12345",
        "训练-very-long-job-name-abcdefghijklmnop",
        "${USER}",
        "PD",
        "0:00",
        "32",
        "Priority",
        "2030-01-02T03:04:05",
        "compute",
        "(Priority)",
        "1-02:03:04",
    ]
    .join("\x1f");
    f.script(
        "squeue",
        &format!(
            "printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"\nprintf '%s\\n' {}",
            quote(&row)
        ),
    );
    let out = f.run(&["q", "--me", "--start", "-t", "PD", "--width", "80"]);
    success(&out);
    assert!(text(&out).contains("123456789_12345"));
    assert!(text(&out).contains("01-02 03:04"));
    assert!(f.calls().starts_with("--me\n-t\nPD\n--sort\nS\n"));
    assert!(!f.calls().contains("--start\n"));
    assert!(!f.calls().contains("--width"));
    assert!(f.calls().contains("TimeLeft:0"));
    for line in text(&out).lines() {
        assert!(unicode_width::UnicodeWidthStr::width(line) <= 80);
    }
}

#[test]
fn native_json_output_is_untouched() {
    let f = Fixture::new();
    f.output_script("squeue", r#"{"jobs":[]}"#);
    let out = f.run(&["--me", "--json"]);
    success(&out);
    assert_eq!(text(&out), "{\"jobs\":[]}\n");
}

#[test]
fn pager_receives_a_single_path_argument_with_spaces() {
    let f = Fixture::new();
    let path = f.root.join("log with spaces.out");
    fs::write(&path, "hello\n").unwrap();
    let record = format!("42\x1f42\x1f{}\x1f\x1f/work", path.display());
    f.output_script("sacct", &record);
    f.script("less", "printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"");
    let out = f.run(&["out", "42"]);
    success(&out);
    assert_eq!(f.calls(), format!("-R\n+G\n--\n{}\n", path.display()));
    let out = f.run(&["out", "42", "--err", "--path"]);
    success(&out);
    assert_eq!(text(&out), format!("{}\n", path.display()));
}

#[test]
fn pending_accounting_falls_back_to_controller_metadata() {
    let f = Fixture::new();
    f.script("sacct", "printf 'accounting unavailable\\n' >&2; exit 1");
    f.job("RUNNING");
    let out = f.run(&["out", "42", "--path"]);
    success(&out);
    assert_eq!(text(&out), "/shared work/logs/a b-000042.out\n");
}

#[test]
fn array_parent_cannot_silently_select_first_log() {
    let f = Fixture::new();
    f.output_script(
        "sacct",
        "42\x1f42_0\x1f/a\x1f\x1f/w\n43\x1f42_1\x1f/b\x1f\x1f/w",
    );
    f.job("RUNNING");
    let out = f.run(&["out", "42", "--path"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("multiple tasks"));
    let out = f.run(&["out", "42_1", "--path"]);
    success(&out);
    assert_eq!(text(&out), "/b\n");
}

#[test]
fn node_order_and_list_step_are_correct_without_a_tty() {
    let f = Fixture::new();
    f.job("RUNNING");
    f.script(
        "srun",
        "printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"; printf 'containers\\n'",
    );
    let out = f.run(&["sh", "42", "--nodes"]);
    success(&out);
    assert_eq!(
        text(&out),
        "INDEX HOSTNAME\n    0 node-c\n    1 node-a\n    2 node-b\n"
    );
    let out = f.run(&["sh", "42", "1", "--list"]);
    success(&out);
    assert!(f.calls().contains("--nodelist=node-a\n"));
    assert!(!f.calls().contains("--pty\n"));
    assert!(f.calls().contains("--overlap\n"));
    let out = f.run(&["sh", "42", "3", "--list"]);
    assert!(!out.status.success());
    let out = f.run(&["sh", "42", "--list"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("selection requires a terminal"));
}

#[test]
fn pending_jobs_never_launch_a_step() {
    let f = Fixture::new();
    f.job("PENDING");
    let out = f.run(&["sh", "42", "0", "--list"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not RUNNING"));
    assert!(!Path::new(&f.root.join("calls")).exists());
}

#[test]
fn help_aliases_are_consistent_and_never_query_slurm() {
    let f = Fixture::new();
    for command in ["squeue", "scontrol", "sacct", "srun", "less", "enroot"] {
        f.script(
            command,
            "printf '%s\\n' \"$0\" >> \"$SJ_TEST_CALLS\"; exit 91",
        );
    }
    for scope in [
        None,
        Some("q"),
        Some("out"),
        Some("script"),
        Some("sh"),
        Some("config"),
    ] {
        let base: Vec<_> = scope.into_iter().collect();
        let mut short = base.clone();
        short.push("-h");
        let mut long = base.clone();
        long.push("--help");
        let mut named = vec!["help"];
        named.extend(base);
        let short = f.run(&short);
        let long = f.run(&long);
        let named = f.run(&named);
        for output in [&short, &long, &named] {
            success(output);
            assert!(output.stderr.is_empty());
            for heading in ["Usage:\n", "Options:\n", "Examples:\n"] {
                assert!(text(output).contains(heading));
            }
        }
        assert_eq!(short.stdout, long.stdout);
        assert_eq!(short.stdout, named.stdout);
        let help = text(&short);
        match scope {
            None => {
                assert!(help.contains("Commands:\n"));
                for example in [
                    "sj q --me",
                    "sj out 12345",
                    "sj script 12345",
                    "sj sh 12345",
                ] {
                    assert!(help.contains(example));
                }
                assert!(!help.contains("__node"));
            }
            Some("q") => {
                for option in [
                    "--me",
                    "--start",
                    "--width",
                    "--noheader",
                    "--user",
                    "--states",
                ] {
                    assert!(help.contains(option));
                }
                assert!(help.contains("sj q --me --noheader"));
            }
            Some("out") => {
                assert!(help.contains("sj out 12345 --err"));
                assert!(help.contains("sj out 12345 --path"));
            }
            Some("sh") => {
                assert!(help.contains("sj sh 12345 0 --host"));
                assert!(help.contains("sj sh 12345 0 --list"));
                assert!(help.contains("--container NAME_OR_PID"));
            }
            Some("script") => {
                assert!(help.contains("--raw"));
                assert!(help.contains("sj script 12345 > job.sbatch"));
            }
            Some("config") => assert!(help.contains("sj config check --config ./sj.toml")),
            _ => unreachable!(),
        }
    }
    for args in [
        vec!["q", "--me", "-h"],
        vec!["out", "12345", "--help"],
        vec!["script", "12345", "-h"],
        vec!["sh", "12345", "0", "-h"],
    ] {
        success(&f.run(&args));
    }
    assert!(!f.root.join("calls").exists());
}

#[test]
fn noheader_is_still_available_after_reserving_short_h_for_help() {
    let f = Fixture::new();
    let row = [
        "42",
        "example",
        "${USER}",
        "R",
        "1:00",
        "1",
        "None",
        "2030-01-02T03:04:05",
        "compute",
        "node-0",
        "UNLIMITED",
    ]
    .join("\x1f");
    f.script(
        "squeue",
        &format!(
            "printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"\nprintf '%s\\n' {}",
            quote(&row)
        ),
    );
    let out = f.run(&["q", "--me", "--noheader"]);
    success(&out);
    assert!(text(&out).contains("example"));
    assert!(!text(&out).contains("JOBID"));
    assert!(f.calls().lines().any(|arg| arg == "--noheader"));
    assert!(!f.calls().lines().any(|arg| arg == "-h"));
    let shortcut = f.run(&["--me", "--noheader"]);
    success(&shortcut);
    assert_eq!(shortcut.stdout, out.stdout);
}

#[test]
fn unknown_help_targets_fail_without_running_a_command() {
    let f = Fixture::new();
    let out = f.run(&["help", "unknown"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("use sj -h"));
}

#[test]
fn script_stdout_is_the_original_copy_including_non_utf8_and_no_final_newline() {
    let f = Fixture::new();
    let original = b"#!/bin/sh\n#SBATCH --job-name=example\n  printf '\xff'\n# final line";
    f.submitted_script(52, original);
    for args in [vec!["script", "42_3"], vec!["script", "42_3", "--raw"]] {
        let out = f.run(&args);
        success(&out);
        assert_eq!(out.stdout, original);
        assert!(out.stderr.is_empty());
        assert_eq!(f.calls(), "write\nbatch_script\n52\n-\n");
    }
}

#[test]
fn historical_script_uses_a_unique_allocation_and_suppresses_sacct_headings() {
    let f = Fixture::new();
    let original = "#!/bin/sh\n# submitted via --wrap\nprintf 'example\\n'\n";
    fs::write(f.root.join("script"), original).unwrap();
    f.script(
        "scontrol",
        "printf 'controller record expired\\n' >&2; exit 1",
    );
    f.script("sacct", &format!(
        "case \"$*\" in *--batch-script*) printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"; cat {};; *) printf '52\\03742_3\\n53\\03742_4\\n';; esac",
        quote(f.root.join("script").to_str().unwrap())
    ));
    let out = f.run(&["script", "42_3"]);
    success(&out);
    assert_eq!(text(&out), original);
    assert_eq!(f.calls(), "-X\n--noheader\n--batch-script\n--jobs\n52\n");
}

#[test]
fn ambiguous_script_parent_never_fetches_the_first_task() {
    let f = Fixture::new();
    f.output_script("scontrol", r#"{"jobs":[{"job_id":52},{"job_id":53}]}"#);
    f.script("sacct", "case \"$*\" in *--batch-script*) touch \"$SJ_TEST_CALLS\"; exit 91;; *) printf '52\\03742_3\\n53\\03742_4\\n';; esac");
    let out = f.run(&["script", "42"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("specify an exact ID"));
    assert!(!f.root.join("calls").exists());
}

#[test]
fn missing_script_and_permission_errors_are_not_successful_empty_output() {
    for missing in [
        "printf 'NONE\\n'",
        "printf 'permission denied\\n' >&2; exit 1",
        ":",
    ] {
        let f = Fixture::new();
        f.script("scontrol", "if [ \"$1\" = '--json' ]; then printf '%s\\n' '{\"jobs\":[{\"job_id\":42}]}'; else printf 'script retrieval failed\\n' >&2; fi");
        f.script(
            "sacct",
            &format!(
                "case \"$*\" in *--batch-script*) {missing};; *) printf '42\\03742\\n';; esac"
            ),
        );
        let out = f.run(&["script", "42", "--raw"]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(error.contains("script retrieval failed"));
        assert!(error.contains("AccountingStoreFlags=job_script"));
    }
}

#[test]
fn invalid_script_arguments_do_not_query_slurm() {
    let f = Fixture::new();
    f.script("scontrol", "touch \"$SJ_TEST_CALLS\"; exit 91");
    for args in [
        vec!["script"],
        vec!["script", "42", "--path"],
        vec!["script", "42;id"],
        vec!["script", "42", "43"],
    ] {
        let out = f.run(&args);
        assert!(!out.status.success());
    }
    assert!(!f.root.join("calls").exists());
}

#[test]
fn queue_refresh_fetches_new_remaining_times() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let f = Fixture::new();
    let row = [
        "42",
        "example",
        "user",
        "R",
        "0:00",
        "1",
        "None",
        "2030-01-02T03:04:05",
        "compute",
        "node-01",
    ]
    .join("\x1f");
    f.script("squeue", &format!(
        "n=0; if [ -f \"$SJ_TEST_CALLS\" ]; then n=$(cat \"$SJ_TEST_CALLS\"); fi; n=$((n+1)); printf '%s' \"$n\" > \"$SJ_TEST_CALLS\"; printf '%s\\0370:0%s\\n' {} \"$n\"",
        quote(&row)
    ));
    let mut child = Command::new(env!("CARGO_BIN_EXE_sj"))
        .args(["q", "--width", "250", "-i", "1"])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", f.root.join("bin").display()),
        )
        .env("HOME", &f.root)
        .env("XDG_CONFIG_HOME", f.root.join("config"))
        .env("SJ_TEST_CALLS", f.root.join("calls"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut snapshot = String::new();
        for _ in 0..4 {
            if reader.read_line(&mut snapshot).unwrap() == 0 {
                break;
            }
        }
        let _ = sender.send(snapshot);
    });
    let snapshot = receiver.recv_timeout(std::time::Duration::from_secs(5));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let status = child.wait().unwrap();
    worker.join().unwrap();
    let snapshot = snapshot.expect("queue did not refresh within five seconds");
    assert_eq!(status.code(), Some(130));
    assert!(snapshot.contains("TIME_LEFT"));
    assert!(snapshot.contains("0:01"));
    assert!(snapshot.contains("0:02"));
    assert!(!snapshot.contains("Every "));
    assert!(!snapshot.contains("Updated:"));
    assert!(!snapshot.contains('\x1b'));
}

#[test]
fn native_iterated_output_keeps_its_format_and_has_no_status_row() {
    let f = Fixture::new();
    f.script(
        "squeue",
        "printf '%s\\n' \"$@\" > \"$SJ_TEST_CALLS\"; printf '%s\\n' '{\"jobs\":[]}'",
    );
    let out = f.run(&["q", "--json", "-i", "5"]);
    success(&out);
    assert_eq!(text(&out), "{\"jobs\":[]}\n");
    assert_eq!(f.calls(), "--json\n--iterate=5\n");
}

struct QueueWatch {
    child: std::process::Child,
    master: fs::File,
    output: Vec<u8>,
}

impl QueueWatch {
    fn open(fixture: &Fixture, args: &[&str]) -> Self {
        Self::open_env(fixture, args, &[])
    }
    fn open_env(fixture: &Fixture, args: &[&str], overrides: &[(&str, &str)]) -> Self {
        use std::os::fd::FromRawFd;
        use std::process::Stdio;
        let mut master = -1;
        let mut slave = -1;
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
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
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let mut command = Command::new(env!("CARGO_BIN_EXE_sj"));
        command
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", fixture.root.join("bin").display()),
            )
            .env("HOME", &fixture.root)
            .env("XDG_CONFIG_HOME", fixture.root.join("config"))
            .env("TERM", "dumb")
            .env_remove("NO_COLOR")
            .env_remove("SQUEUE_STATES")
            .env_remove("SQUEUE_SORT")
            .env("TZ", "UTC0")
            .env("SJ_TEST_CALLS", fixture.root.join("calls"))
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        for (key, value) in overrides {
            command.env(key, value);
        }
        let child = command.spawn().unwrap();
        Self {
            child,
            master,
            output: Vec::new(),
        }
    }

    fn pump(&mut self) {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        let mut fd = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, 50) } > 0 {
            let mut buffer = [0; 8192];
            if let Ok(length) = self.master.read(&mut buffer) {
                self.output.extend_from_slice(&buffer[..length]);
            }
        }
    }

    fn until_rows(&mut self, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while String::from_utf8_lossy(&self.output)
            .matches("watch-")
            .count()
            < count
        {
            self.pump();
            assert!(
                std::time::Instant::now() < deadline,
                "watch did not update: {}",
                String::from_utf8_lossy(&self.output)
            );
        }
    }

    fn resize(&self, columns: u16) {
        use std::os::fd::AsRawFd;
        let size = libc::winsize {
            ws_row: 24,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) },
            0
        );
    }

    fn finish(&mut self) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            self.pump();
            if let Some(status) = self.child.try_wait().unwrap() {
                self.pump();
                return status.code().unwrap();
            }
            assert!(std::time::Instant::now() < deadline, "watch did not exit");
        }
    }
}

impl Drop for QueueWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn watch_row() -> String {
    [
        "42",
        "watch-example",
        "user",
        "R",
        "0:00",
        "1",
        "None",
        "2030-01-02T03:04:05",
        "compute",
        "node-01",
        "1:00:00",
    ]
    .join("\x1f")
}

#[test]
fn tty_refresh_metadata_updates_and_resizes_with_noheader() {
    let fixture = Fixture::new();
    fixture.output_script("squeue", &watch_row());
    let mut watch = QueueWatch::open(&fixture, &["q", "--me", "-i1", "--noheader"]);
    watch.until_rows(1);
    watch.resize(32);
    watch.until_rows(2);
    unsafe {
        libc::kill(watch.child.id() as i32, libc::SIGINT);
    }
    assert_eq!(watch.finish(), 130);
    let output = String::from_utf8_lossy(&watch.output).replace('\r', "");
    let frames: Vec<_> = output.split("\x1b[H\x1b[2J").skip(1).collect();
    assert_eq!(frames.len(), 2, "{output}");
    let first = frames[0].lines().next().unwrap();
    let second = frames[1].lines().next().unwrap();
    assert!(
        first.starts_with("Every 1s") && first.ends_with("+0000"),
        "{first}"
    );
    let full = first.split("Updated: ").nth(1).unwrap();
    assert_eq!(full.len(), 25);
    assert_eq!(&full[4..5], "-");
    assert_eq!(&full[7..8], "-");
    assert!(second.starts_with("Every 1s"), "{second}");
    let clock = second.split("Updated: ").nth(1).unwrap();
    assert_eq!(clock.len(), 8);
    assert_ne!(&full[11..19], clock);
    assert!(!output.contains("JOBID"));
    for line in frames[1].lines() {
        assert!(
            unicode_width::UnicodeWidthStr::width(line) <= 32,
            "{line:?}"
        );
    }
}

#[test]
fn failed_refresh_preserves_the_last_successful_status_and_frame() {
    for row in [watch_row(), String::new()] {
        let fixture = Fixture::new();
        fixture.script("squeue", &format!(
            "if [ -f \"$SJ_TEST_CALLS\" ]; then printf 'scheduler unavailable\\n' >&2; exit 1; fi; touch \"$SJ_TEST_CALLS\"; printf '%s\\n' {}",
            quote(&row)
        ));
        let mut watch = QueueWatch::open(&fixture, &["q", "-i", "1"]);
        assert_eq!(watch.finish(), 1);
        let output = String::from_utf8_lossy(&watch.output);
        assert_eq!(output.matches("Updated:").count(), 1);
        assert_eq!(output.matches("\x1b[H\x1b[2J").count(), 1);
        assert!(output.contains("JOBID"));
        if !row.is_empty() {
            assert!(output.contains("watch-example"));
        }
        assert!(output.contains("scheduler unavailable"));
    }
}

#[test]
fn script_tty_pager_starts_at_the_top_and_handles_early_exit() {
    use std::fs::File;
    use std::os::fd::FromRawFd;
    use std::process::Stdio;
    for (raw, early_exit, expected) in [(false, false, 17), (false, true, 0), (true, false, 0)] {
        let f = Fixture::new();
        let original = if raw {
            "#!/bin/sh\nexit 0\n".into()
        } else {
            format!("#!/bin/sh\n{}", "# example\n".repeat(50000))
        };
        f.submitted_script(42, original.as_bytes());
        let pager_calls = f.root.join("pager-calls");
        let received = f.root.join("received");
        f.script(
            "less",
            &format!(
                "printf '%s\\n' \"$@\" > {}; {}; exit {expected}",
                quote(pager_calls.to_str().unwrap()),
                if early_exit {
                    ":".into()
                } else {
                    format!("cat > {}", quote(received.to_str().unwrap()))
                }
            ),
        );
        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
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
        let _master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let mut command = Command::new(env!("CARGO_BIN_EXE_sj"));
        command.args(["script", "42"]);
        if raw {
            command.arg("--raw");
        }
        let out = command
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", f.root.join("bin").display()),
            )
            .env("HOME", &f.root)
            .env("XDG_CONFIG_HOME", f.root.join("config"))
            .env("SJ_TEST_CALLS", f.root.join("calls"))
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave))
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if raw {
            assert!(!pager_calls.exists());
        } else {
            assert_eq!(fs::read_to_string(pager_calls).unwrap(), "-N\n+g\n");
            if !early_exit {
                assert_eq!(fs::read(received).unwrap(), original.as_bytes());
            }
        }
    }
}

#[test]
fn config_is_generated_once_and_read_only_commands_create_nothing() {
    let f = Fixture::new();
    f.output_script("squeue", &watch_row());
    let config = f.root.join("config/sj/config.toml");
    for args in [
        vec!["-h"],
        vec!["--version"],
        vec!["config", "path"],
        vec!["config", "views"],
        vec!["q", "--list-fields"],
        vec!["q", "--steps", "--list-fields"],
        vec!["q", "--json"],
    ] {
        success(&f.run(&args));
        assert!(!config.exists(), "{args:?}");
    }
    success(&f.run(&["--me"]));
    let template = fs::read_to_string(&config).unwrap();
    assert!(template.contains("schema_version = 1"));
    let patch = "default_view='compact'\n# keep my configuration\n";
    fs::write(&config, patch).unwrap();
    success(&f.run(&["--me"]));
    assert_eq!(fs::read_to_string(&config).unwrap(), patch);
    fs::write(&config, "schema_version=99").unwrap();
    let out = f.run(&["q"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("schema_version"));
    assert_eq!(fs::read_to_string(config).unwrap(), "schema_version=99");
}

#[test]
fn simultaneous_first_runs_publish_a_complete_configuration() {
    let f = Fixture::new();
    f.output_script("squeue", &watch_row());
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| success(&f.run(&["q"]))))
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let config = fs::read_to_string(f.root.join("config/sj/config.toml")).unwrap();
    assert_eq!(config, include_str!("../config/default.toml"));
    assert_eq!(fs::read_dir(f.root.join("config/sj")).unwrap().count(), 1);
}

#[test]
fn explicit_config_errors_and_default_creation_failure_are_distinct() {
    let f = Fixture::new();
    f.output_script("squeue", &watch_row());
    let out = f.run(&["q", "--config", "/missing-sj-config/config.toml"]);
    assert!(!out.status.success());
    assert!(!f.root.join("config/sj/config.toml").exists());
    let blocked = f.root.join("blocked");
    fs::write(&blocked, "file").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sj"))
        .args(["q", "--me"])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", f.root.join("bin").display()),
        )
        .env("HOME", &f.root)
        .env("XDG_CONFIG_HOME", &blocked)
        .output()
        .unwrap();
    success(&out);
    assert!(text(&out).contains("watch-example"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning:"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("built-in configuration"));
}

#[test]
fn named_views_native_fields_and_step_contexts() {
    let f = Fixture::new();
    f.output_script("squeue", &watch_row());
    let config = f.root.join("custom.toml");
    fs::write(&config, "default_view='monitor'\n[columns.Account]\ntitle='PROJECT'\nalign='right'\n[views.monitor]\norder=['jobid','Account','QOS','WorkDir']\n").unwrap();
    let path = config.to_str().unwrap();
    for args in [
        vec!["q", "--config", path],
        vec!["--config", path, "q"],
        vec!["--config", path],
    ] {
        let out = f.run(&args);
        success(&out);
        let text = text(&out);
        assert!(text.contains("PROJECT"));
        assert!(text.contains("research"));
        assert!(text.contains("normal"));
        assert!(text.contains("/work"));
    }
    let steps = f.run(&["q", "--steps=42", "--config", path]);
    success(&steps);
    assert!(text(&steps).contains("STEPID"));
    assert!(text(&steps).contains("42.0"));
    let explicit = f.run(&["q", "--config", path, "--view", "compact", "--start"]);
    success(&explicit);
    assert!(!text(&explicit).contains("START_TIME"));
    fs::write(&config, "[views.steps]\norder=['StepId','TimeLeft']").unwrap();
    let bad = f.run(&["q", "--steps", "--config", path]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown step field"));
}

#[test]
fn config_check_and_views_do_not_query_jobs_or_create_files() {
    let f = Fixture::new();
    f.script("squeue", "printf 'job query is forbidden\\n' >&2; exit 91");
    let config = f.root.join("custom.toml");
    fs::write(&config, "[views.monitor]\norder=['JobId','Account','QOS']").unwrap();
    let path = config.to_str().unwrap();
    success(&f.run(&["config", "check", "--config", path]));
    let views = f.run(&["--config", path, "config", "views"]);
    success(&views);
    assert!(text(&views).contains("monitor\n"));
    assert!(!f.root.join("config/sj/config.toml").exists());
    for patch in [
        "[columns.Typo]\ntitle='BAD'",
        "[views.monitor]\norder=['CPUs']",
    ] {
        fs::write(&config, patch).unwrap();
        let bad = f.run(&["config", "check", "--config", path]);
        assert!(!bad.status.success());
        let error = String::from_utf8_lossy(&bad.stderr);
        assert!(error.contains("custom.toml"));
        assert!(error.contains("field"));
    }
}

#[test]
fn explicit_native_output_ignores_config_and_rejects_a_view() {
    let f = Fixture::new();
    f.output_script("squeue", "native");
    let out = f.run(&[
        "q",
        "--json",
        "--config",
        "/missing-sj-config/config.toml",
        "--color",
        "always",
    ]);
    success(&out);
    assert_eq!(text(&out), "native\n");
    let bad = f.run(&["q", "--json", "--view", "compact"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("cannot be combined"));
    assert!(!f.root.join("config/sj/config.toml").exists());
}

#[test]
fn color_policy_and_custom_theme_are_applied_after_layout() {
    let f = Fixture::new();
    f.output_script("squeue", &watch_row());
    let plain = f.run(&["q"]);
    success(&plain);
    assert!(!text(&plain).contains('\x1b'));
    let always = f.run(&["q", "--color", "always"]);
    success(&always);
    assert!(text(&always).contains("\x1b[32mR"));
    let config = f.root.join("custom.toml");
    fs::write(&config, "[theme]\nstate_scope='row'\n[theme.states.RUNNING]\nfg='#010203'\n[views.queue.columns.Name.style]\nfg=123\nbold=true").unwrap();
    let painted = f.run(&["q", "--color=always", "--config", config.to_str().unwrap()]);
    success(&painted);
    assert!(text(&painted).contains("38;2;1;2;3"));
    assert!(text(&painted).contains("38;5;123;1"));
    let never = f.run(&["q", "--color=never", "--config", config.to_str().unwrap()]);
    success(&never);
    assert!(!text(&never).contains('\x1b'));
}

#[test]
fn watch_recalculates_rules_but_reads_config_only_at_startup() {
    let f = Fixture::new();
    f.output_script("squeue", &watch_row());
    let config = f.root.join("config/sj/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "[views.queue]\norder=['JobId','Name']\nrules=[{min_width=100,order=['JobId','Name','Account']}]\n").unwrap();
    let mut watch = QueueWatch::open(&f, &["q", "-i1"]);
    watch.until_rows(1);
    // An invalid edit must not disturb an already running watch.
    fs::write(&config, "this is no longer TOML").unwrap();
    watch.resize(140);
    watch.until_rows(2);
    unsafe {
        libc::kill(watch.child.id() as i32, libc::SIGINT);
    }
    assert_eq!(watch.finish(), 130);
    let output = String::from_utf8_lossy(&watch.output);
    let frames: Vec<_> = output.split("\x1b[H\x1b[2J").skip(1).collect();
    assert!(!frames[0].contains("research"));
    assert!(frames[1].contains("research"));
}

#[test]
fn auto_color_honors_tty_term_and_no_color() {
    let f = Fixture::new();
    f.output_script("squeue", &watch_row());
    for (mode, overrides, expected) in [
        ("auto", vec![("TERM", "xterm-256color")], true),
        ("auto", vec![("TERM", "dumb")], false),
        ("auto", vec![("TERM", "xterm"), ("NO_COLOR", "")], false),
        ("always", vec![("TERM", "dumb"), ("NO_COLOR", "1")], true),
        ("never", vec![("TERM", "xterm")], false),
    ] {
        let mut tty = QueueWatch::open_env(&f, &["q", "--color", mode], &overrides);
        assert_eq!(tty.finish(), 0);
        let text = String::from_utf8_lossy(&tty.output);
        assert_eq!(text.contains('\x1b'), expected, "{mode}: {overrides:?}");
    }
}

fn filter_row(id: &str, name: &str, state: &str, partition: &str) -> String {
    [
        id,
        name,
        "user",
        state,
        "12:34:56",
        "1",
        if state == "PD" { "Priority" } else { "None" },
        "2030-01-02T03:04:05",
        partition,
        "node-01",
        "1-02:03:04",
    ]
    .join("\x1f")
}

#[test]
fn boolean_filters_include_exclude_and_fetch_hidden_fields() {
    let f = Fixture::new();
    f.queue(&[
        filter_row("42", "train-main", "R", "compute"),
        filter_row("43", "eval-one", "PD", "debug-a"),
        filter_row("44", "train-test", "R", "compute"),
        filter_row("45", "misc", "R", "debug-b"),
    ]);
    let expr = r#"(Name =~ "^train" or Partition glob "debug-*") and not (Name glob "*test*") and State == "RUNNING""#;
    for args in [
        vec!["q", "--me", "--view", "compact", "--filter", expr],
        vec!["--filter", expr, "q", "--me", "--view", "compact"],
        vec!["--filter", expr, "--me", "--view", "compact"],
    ] {
        let out = f.run(&args);
        success(&out);
        let shown = text(&out);
        assert!(shown.contains("train-main"));
        assert!(shown.contains("misc"));
        assert!(!shown.contains("eval-one"));
        assert!(!shown.contains("train-test"));
        assert!(!shown.contains("PARTITION"));
        assert!(f.calls().contains("Partition:0"));
        assert!(!f.calls().contains("--filter"));
    }
}

#[test]
fn repeated_filters_are_and_and_quoted_operators_stay_literal() {
    let f = Fixture::new();
    f.queue(&[
        filter_row("42", "train-main", "R", "compute"),
        filter_row("43", "train-test", "R", "compute"),
        filter_row("44", "and or not && || !", "PD", "debug"),
    ]);
    let out = f.run(&[
        "q",
        "--filter",
        "Name glob 'train-*'",
        "--filter=not (Name =~ 'test')",
    ]);
    success(&out);
    assert!(text(&out).contains("train-main"));
    assert!(!text(&out).contains("train-test"));
    assert!(!text(&out).contains("and or not"));
    let out = f.run(&["q", "--filter", r#"Name == "and or not && || !""#]);
    success(&out);
    assert!(text(&out).contains("and or not && || !"));
    assert!(!text(&out).contains("train-main"));
}

#[test]
fn filters_see_raw_values_before_cleanup_and_layout() {
    let f = Fixture::new();
    f.queue(&[
        filter_row("42", " raw\tname ", "R", "compute"),
        filter_row("43", "raw name", "R", "compute"),
    ]);
    let out = f.run(&[
        "q",
        "--filter",
        r#"Name == " raw\tname " and Reason == "None" and TimeUsed == "12:34:56""#,
        "--width",
        "40",
    ]);
    success(&out);
    assert!(text(&out).contains("42"));
    assert!(!text(&out).contains("43"));
    assert!(text(&out).contains("raw name"));
    assert!(!text(&out).contains('\t'));
    f.queue(&[filter_row("42", "训练-42-long-name", "R", "compute")]);
    let out = f.run(&[
        "q",
        "--filter",
        r#"name =~ "^训练-42-long-name$""#,
        "--width",
        "20",
    ]);
    success(&out);
    assert!(text(&out).contains("42"));
}

#[test]
fn filtering_happens_before_width_allocation_and_zero_matches_keep_headers() {
    let f = Fixture::new();
    let short = filter_row("42", "short", "R", "compute");
    f.queue(&[
        short.clone(),
        filter_row(
            "12345678901234567890",
            "a-name-that-must-not-reserve-any-space",
            "R",
            "compute",
        ),
    ]);
    let filtered = f.run(&["q", "--filter", "JobId == '42'", "--width", "80"]);
    success(&filtered);
    f.queue(&[short]);
    let only = f.run(&["q", "--width", "80"]);
    success(&only);
    assert_eq!(filtered.stdout, only.stdout);
    let empty = f.run(&["q", "--filter", "Name == 'absent'"]);
    success(&empty);
    assert_eq!(text(&empty).lines().count(), 1);
    assert!(text(&empty).contains("JOBID"));
    let empty = f.run(&["q", "--filter", "Name == 'absent'", "--noheader"]);
    success(&empty);
    assert!(empty.stdout.is_empty());
}

#[test]
fn view_filters_compose_and_can_be_ignored() {
    let f = Fixture::new();
    f.queue(&[
        filter_row("42", "train-main", "R", "compute"),
        filter_row("43", "train-test", "R", "compute"),
        filter_row("44", "eval-one", "R", "compute"),
    ]);
    let config = f.root.join("filters.toml");
    fs::write(&config, "[views.queue]\nfilter='Name glob \"train-*\"'\n").unwrap();
    let path = config.to_str().unwrap();
    let out = f.run(&[
        "q",
        "--config",
        path,
        "--filter",
        "not (Name glob '*test*')",
    ]);
    success(&out);
    assert!(text(&out).contains("train-main"));
    assert!(!text(&out).contains("train-test"));
    assert!(!text(&out).contains("eval-one"));
    for args in [
        vec!["q", "--config", path, "--no-view-filter"],
        vec!["--no-view-filter", "--config", path, "q"],
    ] {
        let out = f.run(&args);
        success(&out);
        for name in ["train-main", "train-test", "eval-one"] {
            assert!(text(&out).contains(name));
        }
    }
    let out = f.run(&[
        "q",
        "--config",
        path,
        "--no-view-filter",
        "--filter",
        "Name glob 'eval-*'",
    ]);
    success(&out);
    assert!(text(&out).contains("eval-one"));
    assert!(!text(&out).contains("train-main"));
}

#[test]
fn step_filters_validate_the_step_catalog_and_can_ignore_job_only_view_filter() {
    let f = Fixture::new();
    f.queue(&[
        filter_row("42", "worker-one", "R", "compute"),
        filter_row("43", "batch", "R", "compute"),
    ]);
    let config = f.root.join("filters.toml");
    fs::write(
        &config,
        "[views.steps]\norder=['StepId','StepName']\nfilter='StepState == \"RUNNING\"'\n",
    )
    .unwrap();
    let out = f.run(&[
        "q",
        "--steps",
        "--config",
        config.to_str().unwrap(),
        "--filter",
        "StepName glob 'worker-*'",
    ]);
    success(&out);
    assert!(text(&out).contains("worker-one"));
    assert!(!text(&out).contains("batch"));
    assert!(!text(&out).contains("STATE"));
    assert!(f.calls().contains("StepState:0"));
    fs::write(&config, "[views.steps]\nfilter='Name glob \"worker-*\"'\n").unwrap();
    let bad = f.run(&["q", "--steps", "--config", config.to_str().unwrap()]);
    assert!(!bad.status.success());
    let error = String::from_utf8_lossy(&bad.stderr);
    assert!(error.contains("views.steps.filter"));
    assert!(error.contains("unknown step field"));
    success(&f.run(&[
        "q",
        "--steps",
        "--config",
        config.to_str().unwrap(),
        "--no-view-filter",
    ]));
    assert!(
        !f.calls()
            .split([',', '\n'])
            .any(|field| field.starts_with("Name:0"))
    );
}

#[test]
fn bad_filter_syntax_patterns_and_combinations_fail_without_slurm_calls() {
    let f = Fixture::new();
    f.script("squeue", "touch \"$SJ_TEST_CALLS\"; exit 91");
    for args in [
        vec!["q", "--filter"],
        vec!["q", "--filter="],
        vec!["q", "--filter", "Name == 'x' and"],
        vec!["q", "--filter", "Name =~ '['"],
        vec!["q", "--filter", "Name glob '['"],
        vec!["q", "--filter", "Name > '5'"],
        vec!["q", "--filter", "Name == 'x'", "--json"],
        vec!["q", "--filter", "Name == 'x'", "--list-fields"],
        vec!["q", "--no-view-filter", "--yaml"],
        vec!["q", "--no-view-filter", "--list-fields"],
    ] {
        let out = f.run(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("--filter"));
        assert!(!f.root.join("calls").exists());
        assert!(!f.root.join("config/sj/config.toml").exists());
    }
    let help = f.run(&["q", "--filter", "bad syntax", "-h"]);
    success(&help);
    assert!(text(&help).contains("--filter EXPR"));
}

#[test]
fn unknown_filter_fields_in_any_branch_fail_before_job_query() {
    let f = Fixture::new();
    f.script("squeue", "touch \"$SJ_TEST_CALLS\"; exit 91");
    for args in [
        vec!["q", "--filter", "Name == 'x' or Typo == 'y'"],
        vec!["q", "--filter", "StepName == 'worker'"],
        vec!["q", "--steps", "--filter", "TimeLeft == '0:00'"],
    ] {
        let bad = f.run(&args);
        assert!(!bad.status.success());
        let error = String::from_utf8_lossy(&bad.stderr);
        assert!(error.contains("--filter: at character"));
        assert!(error.contains("field"));
        assert!(!f.root.join("calls").exists());
    }
}

#[test]
fn config_check_validates_filter_syntax_and_hidden_fields_without_job_queries() {
    let f = Fixture::new();
    f.script("squeue", "touch \"$SJ_TEST_CALLS\"; exit 91");
    let config = f.root.join("filters.toml");
    for expr in [
        "Name == 'x' and",
        "Name =~ '['",
        "Name glob '['",
        "Name == 'x' or Typo == 'y'",
    ] {
        fs::write(
            &config,
            format!(
                "[views.queue]\nfilter={}\n",
                serde_json::to_string(expr).unwrap()
            ),
        )
        .unwrap();
        let out = f.run(&["config", "check", "--config", config.to_str().unwrap()]);
        assert!(!out.status.success());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(error.contains("filters.toml"), "{error}");
        assert!(error.contains("filter"), "{error}");
        assert!(!f.root.join("calls").exists());
    }
    fs::write(&config, "[views.queue]\nfilter='Account == \"research\"'\n[views.steps]\nfilter='StepName glob \"worker-*\"'\n").unwrap();
    success(&f.run(&["config", "check", "--config", config.to_str().unwrap()]));
    assert!(!f.root.join("calls").exists());
}

#[test]
fn watch_filters_latest_records_and_resize_does_not_change_filter_semantics() {
    let f = Fixture::new();
    let initial = filter_row("42", "watch-main", "R", "compute");
    let excluded = filter_row("43", "watch-test", "R", "compute");
    f.script("squeue", &format!("n=0; if [ -f \"$SJ_TEST_CALLS\" ]; then n=$(cat \"$SJ_TEST_CALLS\"); fi; n=$((n+1)); printf '%s' \"$n\" > \"$SJ_TEST_CALLS\"; if [ \"$n\" -eq 1 ]; then printf '%s\\n' {}; else printf '%s\\n' {}; fi", quote(&format!("{initial}\n{excluded}")), quote(&excluded)));
    let config = f.root.join("config/sj/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        "[views.queue]\nfilter='not (Name glob \"*test*\")'\n",
    )
    .unwrap();
    let mut watch = QueueWatch::open(&f, &["q", "-i1", "--filter", "Partition == 'compute'"]);
    watch.until_rows(1);
    // Running watches keep their compiled configuration, even after edits.
    fs::write(&config, "[views.queue]\nfilter='Name glob \"*test*\"'\n").unwrap();
    watch.resize(32);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while String::from_utf8_lossy(&watch.output)
        .matches("Updated:")
        .count()
        < 2
    {
        watch.pump();
        assert!(std::time::Instant::now() < deadline);
    }
    unsafe {
        libc::kill(watch.child.id() as i32, libc::SIGINT);
    }
    assert_eq!(watch.finish(), 130);
    let output = String::from_utf8_lossy(&watch.output);
    let frames: Vec<_> = output.split("\x1b[H\x1b[2J").skip(1).collect();
    assert!(frames[0].contains("watch-main"));
    assert!(!output.contains("watch-test"));
    assert!(!frames[1].contains("watch-main"));
    assert!(frames[1].contains("JOBID"));
    for line in frames[1].replace('\r', "").lines() {
        assert!(unicode_width::UnicodeWidthStr::width(line) <= 32);
    }
}

#[test]
fn legacy_slurm_without_help_format_keeps_default_and_custom_views_working() {
    let f = Fixture::new();
    f.queue(&[filter_row("42", "worker-demo", "R", "compute")]);
    f.fail_catalog("squeue: unrecognized option '--helpFormat'");
    for args in [
        vec!["q", "--me"],
        vec!["q", "--me", "--start"],
        vec!["q", "--steps"],
        vec![
            "q",
            "--filter",
            "Account == 'research' and Name glob 'worker-*'",
        ],
        vec!["config", "check"],
    ] {
        let out = f.run(&args);
        success(&out);
        if args[0] == "q" {
            assert!(text(&out).contains("worker-demo"));
        }
    }
    for args in [
        vec!["q", "--list-fields"],
        vec!["q", "--steps", "--list-fields"],
    ] {
        let out = f.run(&args);
        success(&out);
        assert!(text(&out).contains("jobid\n"));
        assert!(String::from_utf8_lossy(&out.stderr).contains("compatibility catalog"));
    }
}

#[test]
fn catalog_errors_do_not_silently_activate_legacy_fallback() {
    for error in [
        "squeue: permission denied",
        "squeue: unrecognized option '--other'",
        "squeue: scheduler unavailable",
    ] {
        let f = Fixture::new();
        f.queue(&[watch_row()]);
        f.fail_catalog(error);
        let out = f.run(&["q"]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(error));
        assert!(!f.root.join("calls").exists());
    }
}

#[test]
fn native_required_values_are_never_reinterpreted_as_switches() {
    let f = Fixture::new();
    for option in ["--name", "-n", "--nam"] {
        for value in [
            "--steps",
            "--start",
            "--filter",
            "--json",
            "--no-view-filter",
            "-h",
            "--help",
            "-Sfoo",
            "-tR",
            "--helpFormat",
        ] {
            f.queue(&[filter_row("42", value, "R", "compute")]);
            let expression = format!("Name == {}", serde_json::to_string(value).unwrap());
            let out = f.run(&["q", "--me", option, value, "--filter", &expression]);
            success(&out);
            assert!(
                text(&out).contains(value),
                "{option} {value}: {}",
                text(&out)
            );
            assert!(f.calls().starts_with(&format!("--me\n{option}\n{value}\n")));
        }
    }
    f.queue(&[filter_row("42", "--start", "R", "compute")]);
    success(&f.run(&["q", "--name", "--start", "--start"]));
    assert!(
        f.calls()
            .starts_with("--name\n--start\n--states\nPD\n--sort\nS\n")
    );
    for name in ["-Sfoo", "-tR"] {
        f.queue(&[filter_row("42", name, "R", "compute")]);
        success(&f.run(&["q", "--name", name, "--start"]));
        assert!(f.calls().contains("--states\nPD\n--sort\nS\n"));
    }
}

#[test]
fn pending_time_numeric_seconds_compact_without_losing_values() {
    let f = Fixture::new();
    f.queue(&[watch_row()]);
    let config = f.root.join("pending.toml");
    fs::write(&config, "[views.pending]\norder=['PendingTime']\n[views.pending.columns.PendingTime]\nmax_width=5\n").unwrap();
    let out = f.run(&[
        "q",
        "--config",
        config.to_str().unwrap(),
        "--view",
        "pending",
        "--noheader",
    ]);
    success(&out);
    assert_eq!(text(&out), "1d10h\n");
}

#[test]
fn config_check_rejects_selectors_that_cannot_run_in_their_context() {
    let f = Fixture::new();
    f.script("squeue", "touch \"$SJ_TEST_CALLS\"; exit 91");
    let config = f.root.join("selectors.toml");
    for (selector, patch) in [
        ("default_view", "default_view='steps'"),
        ("start_view", "start_view='steps'"),
        ("steps_view", "steps_view='queue'"),
        (
            "default_view",
            "default_view='custom'\n[views.custom]\norder=['JobId']\nfilter='StepState == \"RUNNING\"'",
        ),
        (
            "start_view",
            "start_view='custom'\n[views.custom]\norder=['JobId']\nfilter='StepName glob \"*\"'",
        ),
    ] {
        fs::write(&config, patch).unwrap();
        let out = f.run(&["config", "check", "--config", config.to_str().unwrap()]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(selector));
        assert!(!f.root.join("calls").exists());
    }
}

#[test]
fn native_alias_prefixes_share_one_semantic_option() {
    let f = Fixture::new();
    f.queue(&[filter_row("42", "--steps", "R", "compute")]);
    for option in ["--par", "--clu", "--use", "--acc", "--nod"] {
        let out = f.run(&[
            "q",
            "--me",
            option,
            "--steps",
            "--filter",
            "Name == 'absent'",
        ]);
        success(&out);
        assert!(f.calls().starts_with(&format!("--me\n{option}\n--steps\n")));
        let out = f.run(&["q", option, "-h"]);
        success(&out);
        assert!(f.calls().starts_with(&format!("{option}\n-h\n")));
        assert!(!text(&out).starts_with("sj q —"));
    }
    let out = f.run(&["q", "--ste", "--filter", "StepName glob '*'"]);
    success(&out);
    assert!(text(&out).contains("STEPID"));
}
