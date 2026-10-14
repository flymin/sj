use crate::util::{self, Result};
use serde_json::Value;
use std::fs::File;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
struct LogRecord {
    id: String,
    raw_id: String,
    stdout: String,
    stderr: String,
    workdir: String,
}

fn accounting(id: &str) -> Result<LogRecord> {
    let text = util::capture(Command::new("sacct").args([
        "-X",
        "--array",
        "--noheader",
        "--parsable2",
        "--delimiter=\x1f",
        "--jobs",
        id,
        "--expand-pattern",
        "--format=JobIDRaw,JobID%100,StdOut,StdErr,WorkDir",
    ]))?;
    let records = parse_records(&text)?;
    choose_record(records, id)
}

fn parse_records(text: &str) -> Result<Vec<LogRecord>> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split('\x1f').collect();
            if fields.len() != 5 {
                return Err("unexpected sacct output; expected five delimited fields".into());
            }
            Ok(LogRecord {
                raw_id: fields[0].trim().into(),
                id: fields[1].trim().into(),
                stdout: fields[2].into(),
                stderr: fields[3].into(),
                workdir: fields[4].into(),
            })
        })
        .collect()
}

fn choose_record(records: Vec<LogRecord>, id: &str) -> Result<LogRecord> {
    // The human JobID identifies array elements; JobIDRaw identifies their individual allocations.
    let exact: Vec<_> = records.iter().filter(|r| r.id == id).collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }
    if records.len() > 1
        && !id.contains(['_', '+'])
        && records
            .iter()
            .any(|r| r.id.starts_with(&format!("{id}_")) || r.id.starts_with(&format!("{id}+")))
    {
        return Err(format!(
            "job {id} has multiple tasks/components ({}); specify an exact ID",
            records
                .iter()
                .take(12)
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let raw: Vec<_> = records.iter().filter(|r| r.raw_id == id).collect();
    if raw.len() == 1 {
        return Ok(raw[0].clone());
    }
    if records.len() == 1 {
        return Ok(records[0].clone());
    }
    Err(format!(
        "no unique accounting record for job {id}; specify an exact array task/component"
    ))
}

fn expanded_pattern(pattern: &str, job: &Value, requested: &str) -> Result<String> {
    let id = util::number(&job["job_id"]).ok_or("scontrol JSON is missing job_id")?;
    let array_id = util::number(&job["array_job_id"])
        .filter(|n| *n > 0)
        .unwrap_or(id);
    let task = util::number(&job["array_task_id"])
        .or_else(|| requested.split_once('_').and_then(|(_, n)| n.parse().ok()));
    let mut chars = pattern.chars().peekable();
    let mut result = String::new();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        let mut digits = String::new();
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            digits.push(chars.next().unwrap());
        }
        let code = chars
            .next()
            .ok_or("incomplete '%' in stdout filename pattern")?;
        let (value, numeric) = match code {
            '%' => ("%".into(), false),
            'j' => (id.to_string(), true),
            'A' => (array_id.to_string(), true),
            'a' => (task.unwrap_or(4294967294).to_string(), true),
            'b' => (
                task.unwrap_or(4294967294).wrapping_rem(10).to_string(),
                true,
            ),
            'x' => (
                job["name"]
                    .as_str()
                    .or_else(|| job["job_name"].as_str())
                    .unwrap_or("")
                    .into(),
                false,
            ),
            'u' => (job["user_name"].as_str().unwrap_or("").into(), false),
            _ => {
                return Err(format!(
                    "cannot expand %{code} without accounting data; retry when sacct is available"
                ));
            }
        };
        let width = if digits.is_empty() {
            0
        } else {
            digits
                .parse::<usize>()
                .map_err(|_| "invalid filename padding")?
                .min(10)
        };
        if numeric && value.len() < width {
            result.extend(std::iter::repeat_n('0', width - value.len()));
        }
        result.push_str(&value);
    }
    Ok(result)
}

fn controller(id: &str) -> Result<LogRecord> {
    let job = util::json_job(id)?;
    let pattern = job["standard_output"].as_str().unwrap_or("");
    let stdout = if pattern.is_empty() {
        return Err(format!("job {id} has no sbatch stdout path"));
    } else {
        expanded_pattern(pattern, &job, id)?
    };
    let stderr = expanded_pattern(job["standard_error"].as_str().unwrap_or(""), &job, id)?;
    Ok(LogRecord {
        id: id.into(),
        raw_id: id.into(),
        stdout,
        stderr,
        workdir: job["current_working_directory"]
            .as_str()
            .unwrap_or("")
            .into(),
    })
}

fn log_path(record: &LogRecord, stderr: bool) -> Result<PathBuf> {
    let path = if stderr && !record.stderr.is_empty() {
        &record.stderr
    } else {
        &record.stdout
    };
    if path.is_empty() || matches!(path.as_str(), "Unknown" | "(null)" | "N/A") {
        return Err(format!(
            "job {} has no recorded sbatch output path",
            record.id
        ));
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return Ok(path);
    }
    let workdir = PathBuf::from(&record.workdir);
    if !workdir.is_absolute() {
        return Err("relative output path has no absolute WorkDir in job metadata".into());
    }
    Ok(workdir.join(path))
}

pub fn run(args: Vec<String>) -> Result<i32> {
    if crate::help::show_if_requested(&args, crate::help::OUTPUT)? {
        return Ok(0);
    }
    let mut id = None;
    let mut stderr = false;
    let mut path_only = false;
    for arg in args {
        match arg.as_str() {
            "--err" => stderr = true,
            "--path" => path_only = true,
            a if a.starts_with('-') => return Err(format!("unknown out option {a:?}")),
            _ if id.is_none() => id = Some(util::job_id(&arg)?),
            _ => return Err("usage: sj out JOBID [--err] [--path]".into()),
        }
    }
    let id = id.ok_or("usage: sj out JOBID [--err] [--path]")?;
    let record = match accounting(&id) {
        Ok(record) => record,
        Err(account_error) => {
            // Ambiguous arrays must never silently fall back to their first element.
            if account_error.contains("multiple tasks/components") {
                return Err(account_error);
            }
            controller(&id).map_err(|control_error| format!("cannot resolve output for job {id}\n  accounting: {account_error}\n  controller: {control_error}"))?
        }
    };
    let path = log_path(&record, stderr)?;
    if path_only {
        util::print(&format!("{}\n", path.display()))?;
        return Ok(0);
    }
    File::open(&path).map_err(|e| format!("cannot read {}: {e}; the job may not have created the file yet, or the path may not be mounted here", path.display()))?;
    if !path.is_file() {
        return Err(format!("{} is not a regular log file", path.display()));
    }
    eprintln!("sj: {}", path.display());
    util::exec(Command::new("less").args(["-R", "+G", "--"]).arg(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spaces_percent_and_relative_paths() {
        let text =
            ["44", "44", "logs/a b%literal.out", "", "/work with spaces"].join("\x1f") + "\n";
        let r = choose_record(parse_records(&text).unwrap(), "44").unwrap();
        assert_eq!(
            log_path(&r, false).unwrap(),
            PathBuf::from("/work with spaces/logs/a b%literal.out")
        );
        assert_eq!(log_path(&r, true).unwrap(), log_path(&r, false).unwrap());
    }
    #[test]
    fn expands_fallback_without_reexpanding_literals() {
        let job = serde_json::json!({"job_id": 52, "array_job_id": {"set":true,"number":42}, "array_task_id":{"set":true,"number":3}, "name":"a b", "user_name":"user"});
        assert_eq!(
            expanded_pattern("%A_%02a_%j_%x_%u_%%j", &job, "42_3").unwrap(),
            "42_03_52_a b_user_%j"
        );
        assert!(expanded_pattern("%N", &job, "42_3").is_err());
    }
    #[test]
    fn rejects_ambiguous_parent_and_accepts_element_or_raw_id() {
        let text = ["42\x1f42_0\x1f/a\x1f\x1f/w", "43\x1f42_1\x1f/b\x1f\x1f/w"].join("\n");
        let records = parse_records(&text).unwrap();
        assert!(choose_record(records.clone(), "42").is_err());
        assert_eq!(choose_record(records.clone(), "42_1").unwrap().stdout, "/b");
        assert_eq!(choose_record(records, "43").unwrap().stdout, "/b");
    }
}
