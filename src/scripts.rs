use crate::util::{self, Result};
use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};

const USAGE: &str = "usage: sj script JOBID [--raw]";

fn capture_script(command: &mut Command) -> Result<Vec<u8>> {
    let name = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{name} exited with {}: {}",
            util::exit_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // Some scontrol versions return success even when retrieval fails.
    if output.stdout.is_empty() || output.stdout == b"NONE\n" || output.stdout == b"NONE" {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(if detail.trim().is_empty() {
            format!("{name} has no stored batch script")
        } else {
            format!("{name}: {}", detail.trim())
        });
    }
    Ok(output.stdout)
}

fn controller(id: &str) -> Result<Vec<u8>> {
    let job = util::json_job(id)?;
    let raw = util::number(&job["job_id"]).ok_or("scontrol JSON is missing job_id")?;
    capture_script(Command::new("scontrol").args(["write", "batch_script", &raw.to_string(), "-"]))
}

fn allocation(text: &str, requested: &str) -> Result<String> {
    let records: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split('\x1f').map(str::trim).collect();
            if fields.len() != 2 || fields[0].parse::<u32>().ok().filter(|n| *n > 0).is_none() {
                return Err("unexpected script accounting record; expected allocation IDs".into());
            }
            Ok((fields[0], fields[1]))
        })
        .collect::<Result<_>>()?;
    // Never select a parent record while silently ignoring its array tasks/components.
    if !requested.contains(['_', '+'])
        && records.len() > 1
        && records.iter().any(|(_, id)| {
            id.starts_with(&format!("{requested}_")) || id.starts_with(&format!("{requested}+"))
        })
    {
        return Err(format!(
            "job {requested} identifies multiple tasks/components; specify an exact ID"
        ));
    }
    let exact: Vec<_> = records.iter().filter(|(_, id)| *id == requested).collect();
    let matching: Vec<_> = if exact.is_empty() {
        records
            .iter()
            .filter(|(raw, _)| *raw == requested)
            .collect()
    } else {
        exact
    };
    match matching.as_slice() {
        [(raw, _)] => Ok((*raw).into()),
        [] if records.len() == 1
            && (records[0].1.starts_with(&format!("{requested}_"))
                || records[0].1.starts_with(&format!("{requested}+"))) =>
        {
            Ok(records[0].0.into())
        }
        [] => Err(format!("no accounting allocation for job {requested}")),
        _ => Err(format!(
            "job {requested} identifies multiple accounting records; specify an exact ID"
        )),
    }
}

fn accounting(id: &str) -> Result<Vec<u8>> {
    let text = util::capture(Command::new("sacct").args([
        "-X",
        "--array",
        "--noheader",
        "--parsable2",
        "--delimiter=\x1f",
        "--jobs",
        id,
        "--format=JobIDRaw,JobID%100",
    ]))?;
    let raw = allocation(&text, id)?;
    capture_script(Command::new("sacct").args([
        "-X",
        "--noheader",
        "--batch-script",
        "--jobs",
        &raw,
    ]))
}

fn write_stdout(script: &[u8]) -> Result<i32> {
    match io::stdout().lock().write_all(script) {
        Ok(()) => Ok(0),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(0),
        Err(e) => Err(format!("write script: {e}")),
    }
}

fn page(script: &[u8]) -> Result<i32> {
    let mut child = Command::new("less")
        .args(["-N", "+g"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run less: {e}; use --raw for plain output"))?;
    let result = child.stdin.take().unwrap().write_all(script);
    let status = child.wait().map_err(|e| format!("wait for less: {e}"))?;
    if let Err(error) = result
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(format!("send script to less: {error}"));
    }
    Ok(util::exit_code(status))
}

pub fn run(args: Vec<String>) -> Result<i32> {
    if crate::help::show_if_requested(&args, crate::help::SCRIPT)? {
        return Ok(0);
    }
    let mut id = None;
    let mut raw = false;
    for arg in args {
        match arg.as_str() {
            "--raw" => raw = true,
            a if a.starts_with('-') => return Err(format!("unknown script option {a:?}")),
            _ if id.is_none() => id = Some(util::job_id(&arg)?),
            _ => return Err(USAGE.into()),
        }
    }
    let id = id.ok_or(USAGE)?;
    let script = match controller(&id) {
        Ok(script) => script,
        Err(control_error) => accounting(&id).map_err(|account_error| {
            format!("cannot retrieve submitted script for job {id}\n  controller: {control_error}\n  accounting: {account_error}\n  Historical scripts require AccountingStoreFlags=job_script and retained records.")
        })?,
    };
    if raw || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        write_stdout(&script)
    } else {
        page(&script)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_exact_array_and_component_allocations() {
        let records = "52\x1f42_3\n53\x1f42_4\n";
        assert_eq!(allocation(records, "42_3").unwrap(), "52");
        assert_eq!(allocation(records, "52").unwrap(), "52");
        assert!(allocation(records, "42").is_err());
        assert_eq!(allocation("52\x1f42+1\n", "42+1").unwrap(), "52");
        assert_eq!(allocation("52\x1f42_3\n", "42").unwrap(), "52");
        assert!(allocation("42\x1f42\n52\x1f42_3\n", "42").is_err());
    }

    #[test]
    fn rejects_missing_duplicate_and_malformed_records() {
        for text in ["", "42\x1f42\n42\x1f42\n", "x\x1f42", "42\x1f42\x1fx"] {
            assert!(allocation(text, "42").is_err());
        }
        assert!(allocation("43\x1f43\n", "42").is_err());
    }
}
