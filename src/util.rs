use serde_json::Value;
use std::io::{self, IsTerminal, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus};

pub type Result<T> = std::result::Result<T, String>;

pub fn print(text: &str) -> Result<()> {
    match io::stdout().lock().write_all(text.as_bytes()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(e) => Err(format!("write stdout: {e}")),
    }
}

pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

pub fn capture(command: &mut Command) -> Result<String> {
    let name = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{name} exited with {}: {}",
            exit_code(output.status),
            detail.trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("invalid UTF-8 from {name}: {e}"))
}

pub fn exec(command: &mut Command) -> Result<i32> {
    let name = command.get_program().to_string_lossy().into_owned();
    Err(format!("cannot execute {name}: {}", command.exec()))
}

pub fn job_id(s: &str) -> Result<String> {
    let mut parts = s.split(['_', '+']);
    let base = parts.next().unwrap_or("");
    if base.is_empty()
        || !base.bytes().all(|c| c.is_ascii_digit())
        || base.parse::<u32>().ok().filter(|n| *n > 0).is_none()
    {
        return Err(format!(
            "invalid job ID {s:?}; use JOBID or JOBID_ARRAY_TASK"
        ));
    }
    if let Some(part) = parts.next()
        && (part.is_empty()
            || !part.bytes().all(|c| c.is_ascii_digit())
            || part.parse::<u32>().is_err()
            || parts.next().is_some())
    {
        return Err(format!(
            "invalid job ID {s:?}; specify a single array task or heterogeneous component"
        ));
    }
    Ok(s.to_owned())
}

pub fn json_job(id: &str) -> Result<Value> {
    let text = capture(Command::new("scontrol").args(["--json", "show", "job", id]))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid scontrol JSON: {e}"))?;
    if let Some(errors) = value["errors"].as_array().filter(|e| !e.is_empty()) {
        return Err(format!("scontrol: {}", Value::Array(errors.clone())));
    }
    let jobs = value["jobs"]
        .as_array()
        .ok_or("scontrol JSON has no jobs array")?;
    match jobs.as_slice() {
        [job] => Ok(job.clone()),
        [] => Err(format!("job {id} is not available in scontrol")),
        _ => Err(format!(
            "job {id} identifies multiple records; specify an array task or heterogeneous component"
        )),
    }
}

pub fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        if value["set"].as_bool() == Some(true) && value["infinite"].as_bool() != Some(true) {
            value["number"].as_u64()
        } else {
            None
        }
    })
}

pub fn is_running(job: &Value) -> bool {
    job["job_state"]
        .as_array()
        .is_some_and(|states| states.iter().any(|s| s == "RUNNING"))
        || job["job_state"].as_str().is_some_and(|s| s == "RUNNING")
}

pub fn hostnames(nodelist: &str) -> Result<Vec<String>> {
    if nodelist.is_empty() {
        return Err("job has no allocated nodes".into());
    }
    let text = capture(Command::new("scontrol").args(["show", "hostnames", nodelist]))?;
    let hosts: Vec<String> = text
        .lines()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if hosts.is_empty() {
        return Err("scontrol returned no allocated nodes".into());
    }
    Ok(hosts)
}

pub fn require_terminal() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        Err("an interactive shell requires a terminal; use --nodes or --list for noninteractive output".into())
    } else {
        Ok(())
    }
}

pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_ids_only() {
        for id in ["12345", "42_0", "42+1"] {
            assert!(job_id(id).is_ok());
        }
        for id in [
            "",
            "0",
            "-1",
            "42;id",
            "42.0",
            "42_",
            "42_1+2",
            "42_1_2",
            "99999999999999",
        ] {
            assert!(job_id(id).is_err(), "{id}");
        }
    }
}
