use crate::queue;
use crate::util::{self, Result};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Container {
    pub name: String,
    pub pid: u32,
    pub namespace: PathBuf,
    pub start_time: String,
    pub job: Option<String>,
    pub command: String,
}

fn process_start(proc: &Path) -> Option<String> {
    let stat = fs::read_to_string(proc.join("stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    if matches!(fields.first().copied(), Some("Z" | "X")) {
        return None;
    }
    fields.get(19).map(|s| (*s).into())
}

fn unescape_mount(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4].iter().all(|b| matches!(b, b'0'..=b'7'))
        {
            let n = ((bytes[i + 1] - b'0') as u16) * 64
                + ((bytes[i + 2] - b'0') as u16) * 8
                + (bytes[i + 3] - b'0') as u16;
            result.push(n as u8);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn container_name(mountinfo: &str) -> String {
    for line in mountinfo.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() > 5 && fields[4] == "/" {
            let root = unescape_mount(fields[3]);
            if let Some(name) = Path::new(&root).file_name().and_then(|s| s.to_str()) {
                return util::clean(name);
            }
        }
    }
    "<unknown>".into()
}

fn job_from_cgroup(text: &str) -> Option<String> {
    text.split('/')
        .filter_map(|part| part.strip_prefix("job_"))
        .find_map(|part| {
            let id = part.trim_end_matches(char::is_whitespace);
            if !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()) {
                Some(id.into())
            } else {
                None
            }
        })
}

fn job_from_environment(bytes: &[u8]) -> Option<String> {
    for field in bytes.split(|b| *b == 0) {
        if let Some(value) = field.strip_prefix(b"SLURM_JOB_ID=") {
            return std::str::from_utf8(value)
                .ok()
                .and_then(|s| util::job_id(s).ok());
        }
    }
    None
}

pub fn discover() -> Result<Vec<Container>> {
    // Enroot 4.x marks its root with /.lock. Scan namespaces rather than the
    // configured rootfs directory: Pyxis may use a different ENROOT_DATA_PATH.
    let uid = unsafe { libc::geteuid() };
    discover_at(Path::new("/proc"), uid)
}

fn discover_at(root: &Path, uid: u32) -> Result<Vec<Container>> {
    let own_namespace = fs::read_link(root.join("self/ns/mnt"))
        .map_err(|e| format!("read mount namespace: {e}"))?;
    let mut groups: BTreeMap<PathBuf, Vec<u32>> = BTreeMap::new();
    for entry in fs::read_dir(root)
        .map_err(|e| format!("scan /proc: {e}"))?
        .flatten()
    {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let proc = entry.path();
        if !fs::metadata(&proc).is_ok_and(|m| m.uid() == uid) {
            continue;
        }
        let Ok(namespace) = fs::read_link(proc.join("ns/mnt")) else {
            continue;
        };
        if namespace == own_namespace {
            continue;
        }
        groups.entry(namespace).or_default().push(pid);
    }
    let mut containers = Vec::new();
    for (namespace, mut pids) in groups {
        pids.sort_unstable();
        for pid in pids {
            let proc = root.join(pid.to_string());
            if !proc.join("root/.lock").is_file() {
                continue;
            }
            let Some(start_time) = process_start(&proc) else {
                continue;
            };
            let Ok(mountinfo) = fs::read_to_string(proc.join("mountinfo")) else {
                continue;
            };
            let name = container_name(&mountinfo);
            let job = fs::read_to_string(proc.join("cgroup"))
                .ok()
                .and_then(|s| job_from_cgroup(&s))
                .or_else(|| {
                    fs::read(proc.join("environ"))
                        .ok()
                        .and_then(|b| job_from_environment(&b))
                });
            let command = fs::read_to_string(proc.join("comm"))
                .map(|s| util::clean(s.trim()))
                .unwrap_or_default();
            containers.push(Container {
                name,
                pid,
                namespace,
                start_time,
                job,
                command,
            });
            break;
        }
    }
    containers.sort_by(|a, b| a.name.cmp(&b.name).then(a.pid.cmp(&b.pid)));
    Ok(containers)
}

pub fn verify(container: &Container) -> Result<()> {
    let proc = PathBuf::from(format!("/proc/{}", container.pid));
    let uid = unsafe { libc::geteuid() };
    let valid = fs::metadata(&proc).is_ok_and(|m| m.uid() == uid)
        && process_start(&proc).as_deref() == Some(container.start_time.as_str())
        && fs::read_link(proc.join("ns/mnt")).ok().as_ref() == Some(&container.namespace)
        && proc.join("root/.lock").is_file();
    if !valid {
        return Err(format!(
            "container {} (PID {}) has exited or changed; list containers again",
            container.name, container.pid
        ));
    }
    Ok(())
}

pub fn find<'a>(containers: &'a [Container], name_or_pid: &str) -> Result<&'a Container> {
    // A numeric selector always means a host PID, never a menu index.
    let matches: Vec<_> = if let Ok(pid) = name_or_pid.parse::<u32>() {
        containers.iter().filter(|c| c.pid == pid).collect()
    } else {
        containers
            .iter()
            .filter(|c| c.name == name_or_pid)
            .collect()
    };
    match matches.as_slice() {
        [container] => Ok(*container),
        [] => Err(format!(
            "no running Enroot container matches {name_or_pid:?}; use --list"
        )),
        _ => Err(format!(
            "multiple running containers are named {name_or_pid:?}; specify a PID from --list"
        )),
    }
}

pub fn table(containers: &[Container]) -> String {
    if containers.is_empty() {
        return "No running Enroot containers visible for the current user.\n".into();
    }
    let width = queue::terminal_width();
    let name_width = containers
        .iter()
        .map(|c| unicode_width::UnicodeWidthStr::width(c.name.as_str()))
        .max()
        .unwrap_or(4)
        .max(4)
        .min(width.saturating_sub(38).clamp(8, 50));
    let mut out = format!(
        "{:>5} {:name_width$} {:>10} {:>10} {}\n",
        "INDEX", "NAME", "PID", "JOB", "COMM"
    );
    for (i, c) in containers.iter().enumerate() {
        let name = queue::clip(&c.name, name_width);
        let padding =
            name_width.saturating_sub(unicode_width::UnicodeWidthStr::width(name.as_str()));
        out.push_str(&format!(
            "{i:>5} {name}{} {:>10} {:>10} {}\n",
            " ".repeat(padding),
            c.pid,
            c.job.as_deref().unwrap_or("?"),
            c.command
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake(name: &str, pid: u32) -> Container {
        Container {
            name: name.into(),
            pid,
            namespace: "mnt:[1]".into(),
            start_time: "10".into(),
            job: Some("42".into()),
            command: "bash".into(),
        }
    }
    #[test]
    fn extracts_container_root_name_and_job() {
        assert_eq!(
            container_name(
                "100 99 0:1 /enroot-cache/data/user-1000/container-demo / rw - ext4 /dev/test rw"
            ),
            "container-demo"
        );
        assert_eq!(
            container_name("1 2 0:1 /enroot/a\\040b / rw - ext4 /dev/x rw"),
            "a b"
        );
        assert_eq!(
            job_from_cgroup("0::/slurm/uid_1000/job_12345/step_0/task_0\n"),
            Some("12345".into())
        );
        assert_eq!(
            job_from_environment(b"SECRET=hidden\0SLURM_JOB_ID=42\0"),
            Some("42".into())
        );
    }
    #[test]
    fn ambiguity_and_pid_reuse() {
        let containers = vec![fake("a", 20), fake("a", 21)];
        assert!(find(&containers, "a").is_err());
        assert_eq!(find(&containers, "20").unwrap().pid, 20);
        assert!(find(&containers, "0").is_err());
        assert!(verify(&fake("gone", u32::MAX)).is_err());
    }

    #[test]
    fn scan_groups_live_processes_and_excludes_stopped_filesystems() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("sj-proc-test-{}", std::process::id()));
        fs::create_dir_all(root.join("self/ns")).unwrap();
        symlink("mnt:[999]", root.join("self/ns/mnt")).unwrap();
        for (pid, namespace, state, marked) in [
            (10, 42, "S", true),
            (20, 42, "S", true),
            (30, 43, "Z", true),
            (40, 44, "S", false),
            (50, 999, "S", true),
        ] {
            let proc = root.join(pid.to_string());
            fs::create_dir_all(proc.join("ns")).unwrap();
            fs::create_dir_all(proc.join("root")).unwrap();
            symlink(format!("mnt:[{namespace}]"), proc.join("ns/mnt")).unwrap();
            if marked {
                fs::write(proc.join("root/.lock"), "").unwrap();
            }
            fs::write(
                proc.join("stat"),
                format!("{pid} (bash) {state} {} 10", "0 ".repeat(18)),
            )
            .unwrap();
            fs::write(
                proc.join("mountinfo"),
                "1 2 0:1 /enroot/container-test / rw - ext4 /dev/test rw",
            )
            .unwrap();
            fs::write(proc.join("cgroup"), "0::/slurm/job_42/step_0/task_0\n").unwrap();
            fs::write(proc.join("comm"), "bash\n").unwrap();
        }
        fs::create_dir_all(root.join("stopped-container")).unwrap();
        let found = discover_at(&root, unsafe { libc::geteuid() }).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 10);
        assert_eq!(found[0].name, "container-test");
        assert_eq!(found[0].job.as_deref(), Some("42"));
    }
}
