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
        fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn output_script(&self, name: &str, text: &str) {
        self.script(name, &format!("printf '%s\\n' {}", quote(text)));
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sj"))
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
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
    assert!(f.calls().starts_with("--me\n--start\n-t\nPD\n"));
    assert!(!f.calls().contains("--width"));
    assert!(f.calls().contains("%L"));
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
    for scope in [None, Some("q"), Some("out"), Some("script"), Some("sh")] {
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
