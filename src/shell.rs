use crate::containers::{self, Container};
use crate::terminal;
use crate::util::{self, Result};
use std::env;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Choose,
    Host,
    List,
    Container(String),
    Nodes,
}

#[derive(Debug)]
struct Options {
    id: String,
    index: Option<usize>,
    mode: Mode,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut id = None;
    let mut index = None;
    let mut mode = Mode::Choose;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let selected = match a {
            "--nodes" => Some(Mode::Nodes),
            "--host" => Some(Mode::Host),
            "--list" => Some(Mode::List),
            "--container" => { i += 1; Some(Mode::Container(args.get(i).ok_or("--container requires a name or PID")?.clone())) },
            a if a.starts_with("--container=") => Some(Mode::Container(a[12..].into())),
            a if a.starts_with('-') => return Err(format!("unknown sh option {a:?}")),
            _ if id.is_none() => { id = Some(util::job_id(a)?); None },
            _ if index.is_none() => { index = Some(a.parse().map_err(|_| "NODE_INDEX must be a nonnegative integer")?); None },
            _ => return Err("usage: sj sh JOBID [NODE_INDEX] [--host | --list | --container NAME_OR_PID | --nodes]".into()),
        };
        if let Some(selected) = selected {
            if mode != Mode::Choose {
                return Err(
                    "--host, --list, --container and --nodes are mutually exclusive".into(),
                );
            }
            if matches!(&selected, Mode::Container(name) if name.is_empty()) {
                return Err("--container requires a name or PID".into());
            }
            mode = selected;
        }
        i += 1;
    }
    Ok(Options {
        id: id.ok_or("usage: sj sh JOBID [NODE_INDEX] [options]")?,
        index,
        mode,
    })
}

fn node_table(nodes: &[String]) -> String {
    let mut out = String::from("INDEX HOSTNAME\n");
    for (i, host) in nodes.iter().enumerate() {
        out.push_str(&format!("{i:>5} {host}\n"));
    }
    out
}

fn container_choices(containers: &[Container]) -> Vec<String> {
    let mut choices: Vec<_> = containers
        .iter()
        .map(|c| {
            format!(
                "{} (PID {}, job {})",
                c.name,
                c.pid,
                c.job.as_deref().unwrap_or("?")
            )
        })
        .collect();
    choices.push("host shell".into());
    choices
}

fn step_command(executable: &Path, job: u64, host: &str, mode: &Mode) -> Command {
    let mut command = Command::new("srun");
    command.args([format!("--jobid={job}"), format!("--nodelist={host}")]);
    command.args([
        "--nodes=1",
        "--ntasks=1",
        "--ntasks-per-node=1",
        "--cpus-per-task=1",
        "--overlap",
        "--exact",
        "--mpi=none",
        "--cpu-bind=none",
        "--export=ALL",
        "--immediate=10",
    ]);
    if *mode != Mode::List {
        command.arg("--pty");
    }
    command.arg(executable).arg("__node").arg(job.to_string());
    match mode {
        Mode::Host => {
            command.arg("--host");
        }
        Mode::List => {
            command.arg("--list");
        }
        Mode::Container(name) => {
            command.args(["--container", name]);
        }
        _ => (),
    }
    command
}

pub fn run(args: Vec<String>) -> Result<i32> {
    if crate::help::show_if_requested(&args, crate::help::SHELL)? {
        return Ok(0);
    }
    let options = parse(&args)?;
    let job = util::json_job(&options.id)?;
    if !util::is_running(&job) {
        return Err(format!(
            "job {} is not RUNNING (state: {})",
            options.id, job["job_state"]
        ));
    }
    let uid = unsafe { libc::geteuid() } as u64;
    if util::number(&job["user_id"]) != Some(uid) {
        return Err(format!(
            "job {} is not owned by the current user",
            options.id
        ));
    }
    let nodes = util::hostnames(
        job["nodes"]
            .as_str()
            .ok_or("scontrol JSON is missing the original nodes list")?,
    )?;
    if options.mode == Mode::Nodes {
        util::print(&node_table(&nodes))?;
        return Ok(0);
    }
    let index = match options.index {
        Some(index) if index < nodes.len() => index,
        Some(index) => {
            return Err(format!(
                "node index {index} is out of range for {} nodes (valid: 0..{})",
                nodes.len(),
                nodes.len() - 1
            ));
        }
        None if nodes.len() == 1 => 0,
        None => {
            let Some(index) = terminal::select(
                &format!("Select node | job {}", options.id),
                "HOSTNAME",
                &nodes,
            )?
            else {
                return Ok(0);
            };
            index
        }
    };
    if options.mode != Mode::List {
        util::require_terminal()?;
    }
    let host = &nodes[index];
    eprintln!("sj: job {} / node {index} / {host}", options.id);
    let raw_job = util::number(&job["job_id"]).ok_or("scontrol JSON is missing job_id")?;
    let executable = env::current_exe().map_err(|e| format!("locate sj binary: {e}"))?;
    util::exec(&mut step_command(&executable, raw_job, host, &options.mode))
}

fn host_shell() -> Result<i32> {
    let shell = env::var("SHELL")
        .ok()
        .filter(|s| Path::new(s).is_absolute() && Path::new(s).is_file())
        .unwrap_or_else(|| {
            if Path::new("/bin/bash").is_file() {
                "/bin/bash".into()
            } else {
                "/bin/sh".into()
            }
        });
    util::exec(Command::new(shell).arg("-l"))
}

fn enter(container: &Container) -> Result<i32> {
    containers::verify(container)?;
    eprintln!(
        "sj: entering {} (PID {}, job {})",
        container.name,
        container.pid,
        container.job.as_deref().unwrap_or("?")
    );
    // This is a fixed program, without interpolated names or paths. The target
    // environment and cwd are restored by enroot exec itself.
    util::exec(Command::new("enroot").args([
        "exec",
        &container.pid.to_string(),
        "/bin/sh",
        "-c",
        "if [ -x /bin/bash ]; then exec /bin/bash -l; else exec /bin/sh -l; fi",
    ]))
}

pub fn node_run(args: Vec<String>) -> Result<i32> {
    let options = parse(&args)?;
    let actual = env::var("SLURM_JOB_ID")
        .map_err(|_| "internal node helper must run inside an srun job step")?;
    if actual != options.id {
        return Err(format!(
            "node helper job mismatch: expected {}, found {actual}",
            options.id
        ));
    }
    if options.index.is_some() || options.mode == Mode::Nodes {
        return Err("invalid internal node-helper options".into());
    }
    if options.mode == Mode::Host {
        util::require_terminal()?;
        return host_shell();
    }
    let containers = containers::discover()?;
    if options.mode == Mode::List {
        util::print(&containers::table(&containers))?;
        return Ok(0);
    }
    util::require_terminal()?;
    if let Mode::Container(name) = &options.mode {
        return enter(containers::find(&containers, name)?);
    }
    let choices = container_choices(&containers);
    let Some(index) = terminal::select(
        &format!("Select shell | job {}", options.id),
        "TARGET",
        &choices,
    )?
    else {
        return Ok(0);
    };
    if index == containers.len() {
        host_shell()
    } else {
        enter(&containers[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_allocation_order() {
        let nodes = vec!["node-c".into(), "node-a".into(), "node-b".into()];
        assert_eq!(
            node_table(&nodes),
            "INDEX HOSTNAME\n    0 node-c\n    1 node-a\n    2 node-b\n"
        );
    }
    #[test]
    fn argv_uses_one_overlapping_step_and_preserves_selector() {
        let command = step_command(
            Path::new("/shared home/bin/sj"),
            42,
            "node-c",
            &Mode::Container("a;echo bad".into()),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--overlap".into()));
        assert!(args.contains(&"--pty".into()));
        assert!(args.contains(&"--ntasks=1".into()));
        assert!(!args.contains(&"--gres=none".into()));
        assert!(args.ends_with(&[
            "/shared home/bin/sj".into(),
            "__node".into(),
            "42".into(),
            "--container".into(),
            "a;echo bad".into()
        ]));
        let list = step_command(Path::new("/bin/sj"), 42, "node-c", &Mode::List);
        assert!(!list.get_args().any(|a| a == "--pty"));
    }
    #[test]
    fn conflicting_modes_are_rejected() {
        assert!(parse(&["42", "0", "--host", "--list"].map(str::to_owned)).is_err());
        assert!(parse(&["42", "--container="].map(str::to_owned)).is_err());
        assert_eq!(
            parse(&["42", "2", "--container=x"].map(str::to_owned))
                .unwrap()
                .mode,
            Mode::Container("x".into())
        );
    }

    #[test]
    fn containers_are_first_and_host_is_a_real_last_option() {
        let containers = vec![Container {
            name: "container-test".into(),
            pid: 123,
            namespace: "mnt:[1]".into(),
            start_time: "1".into(),
            job: Some("42".into()),
            command: "bash".into(),
        }];
        let choices = container_choices(&containers);
        assert!(choices[0].starts_with("container-test"));
        assert_eq!(choices[containers.len()], "host shell");
        assert_eq!(container_choices(&[]), ["host shell"]);
    }
}
