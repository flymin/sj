use super::{
    model::identity,
    process::{self, TIMEOUT},
};
use crate::{
    fields::Row,
    queue::update_timestamp,
    util::{self, Result},
};
use serde_json::Value;
use std::{collections::BTreeMap, process::Command, sync::atomic::AtomicBool};

#[derive(Debug)]
pub struct Controller {
    pub raw: String,
    pub values: BTreeMap<String, String>,
    pub updated: String,
}
fn normalized(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}
fn scalar(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    if value["infinite"].as_bool() == Some(true) {
        return "UNLIMITED".into();
    }
    if value["set"].as_bool() == Some(false) || value.is_null() {
        return "—".into();
    }
    if let Some(number) = util::number(value) {
        return number.to_string();
    }
    if let Some(values) = value.as_array() {
        return values.iter().map(scalar).collect::<Vec<_>>().join(", ");
    }
    value.to_string()
}
fn field_value(key: &str, value: &Value) -> String {
    if let Some(number) = util::number(value) {
        if matches!(
            key,
            "start_time" | "end_time" | "submit_time" | "eligible_time"
        ) {
            return if number == 0 {
                "—".into()
            } else {
                crate::queue::timestamp(number.try_into().unwrap_or(i64::MAX))
            };
        }
        if matches!(
            key,
            "time_limit" | "time_minimum" | "time_used" | "time_left"
        ) {
            return duration(number.saturating_mul(
                if matches!(key, "time_limit" | "time_minimum") {
                    60
                } else {
                    1
                },
            ));
        }
    }
    scalar(value)
}
fn duration(seconds: u64) -> String {
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}
fn unavailable(value: &Value) -> bool {
    value.is_null()
        || value.as_str() == Some("")
        || (value["set"].as_bool() == Some(false) && value["infinite"].as_bool() != Some(true))
}
fn leaf<'a>(record: &'a Value, paths: &[&str]) -> Option<&'a Value> {
    let mut empty = None;
    for path in paths {
        if let Some(value) = record.pointer(path) {
            if value.is_object() && value.get("set").is_none() {
                continue;
            }
            if unavailable(value) {
                empty = Some(value);
                continue;
            }
            return Some(value);
        }
    }
    empty
}
fn summary(record: &Value) -> BTreeMap<String, String> {
    // Step records use nested time/tres data; job records use top-level fields.
    // Keep one semantic key per summary value, separate from the raw response.
    let fields: &[(&str, &[&str], &str)] = &[
        ("name", &["/name", "/job_name", "/step_name"], ""),
        (
            "username",
            &["/user_name", "/user/name", "/user", "/user_id"],
            "",
        ),
        ("state", &["/job_state", "/step_state", "/state"], ""),
        ("account", &["/account"], ""),
        ("qos", &["/qos"], ""),
        ("partition", &["/partition"], ""),
        ("cluster", &["/cluster"], ""),
        (
            "statereason",
            &["/state_reason", "/state_description", "/reason"],
            "",
        ),
        (
            "timeused",
            &["/time/running", "/run_time", "/time_used"],
            "time_used",
        ),
        ("timeleft", &["/time/remaining", "/time_left"], "time_left"),
        ("timelimit", &["/time/limit", "/time_limit"], "time_limit"),
        ("submittime", &["/submit_time"], "submit_time"),
        ("starttime", &["/time/start", "/start_time"], "start_time"),
        ("endtime", &["/time/end", "/end_time"], "end_time"),
        ("priority", &["/priority"], ""),
        ("dependency", &["/dependency"], ""),
        (
            "tresallocstr",
            &[
                "/tres/allocation",
                "/tres_alloc_str",
                "/tres_req_str",
                "/tres",
            ],
            "",
        ),
        ("cpus", &["/number_cpus", "/cpus", "/num_cpus"], ""),
        (
            "memorypernode",
            &["/memory_per_node", "/memory_per_cpu"],
            "",
        ),
        (
            "nodes",
            &["/nodes/list", "/nodes", "/job_resources/nodes/list"],
            "",
        ),
        (
            "nodecount",
            &["/node_count", "/number_nodes", "/job_resources/nodes/count"],
            "",
        ),
        ("batchhost", &["/batch_host"], ""),
        ("command", &["/command", "/submit_line"], ""),
        (
            "currentworkingdirectory",
            &["/current_working_directory", "/work_dir"],
            "",
        ),
        (
            "standardoutput",
            &["/standard_output", "/stdout_expanded"],
            "",
        ),
        (
            "standarderror",
            &["/standard_error", "/stderr_expanded"],
            "",
        ),
    ];
    let mut values: BTreeMap<_, _> = fields
        .iter()
        .filter_map(|(key, paths, format)| {
            leaf(record, paths).map(|value| ((*key).to_owned(), field_value(format, value)))
        })
        .collect();
    for (path, basis) in [("/memory_per_node", "node"), ("/memory_per_cpu", "CPU")] {
        if let Some(value) = leaf(record, &[path]).filter(|value| !unavailable(value)) {
            let memory = util::number(value)
                .map_or_else(|| scalar(value), |number| format!("{number} MiB/{basis}"));
            values.insert("memorypernode".into(), memory);
            break;
        }
    }
    if !values.contains_key("timeleft") {
        let limit = leaf(record, &["/time/limit", "/time_limit"]);
        let running =
            leaf(record, &["/time/running", "/run_time", "/time_used"]).and_then(util::number);
        if limit.is_some_and(|v| v["infinite"].as_bool() == Some(true)) {
            values.insert("timeleft".into(), "UNLIMITED".into());
        } else if let Some((limit, running)) = limit.and_then(util::number).zip(running) {
            values.insert(
                "timeleft".into(),
                duration(limit.saturating_mul(60).saturating_sub(running)),
            );
        }
    }
    values
}
fn command(cluster: &str, id: &str, json: bool) -> Command {
    let mut command = Command::new("scontrol");
    command.arg(if json { "--json" } else { "--details" });
    if !cluster.is_empty() {
        command.args(["--clusters", cluster]);
    }
    command.args(["show", if id.contains('.') { "step" } else { "job" }, id]);
    command.env("LC_ALL", "C");
    command
}
pub fn task_id(row: &Row) -> Result<(String, String)> {
    let (cluster, id) = identity(row);
    if id.contains('[') || id.contains(']') {
        return Err("Grouped array tasks: use --array to inspect an individual task".into());
    }
    if let Some((job, step)) = id.split_once('.') {
        util::job_id(job)?;
        if step.is_empty() || !step.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err("invalid step ID in queue".into());
        }
    } else {
        util::job_id(&id)?;
    }
    Ok((cluster, id))
}
pub fn fetch(row: &Row, cancel: &AtomicBool) -> Result<Controller> {
    let (cluster, id) = task_id(row)?;
    let text = process::capture(&mut command(&cluster, &id, true), cancel, TIMEOUT);
    let text = match text {
        Ok(text) => text,
        Err(error)
            if error.contains("--json")
                && [
                    "unrecognized option",
                    "unknown option",
                    "not supported",
                    "not available",
                ]
                .iter()
                .any(|part| error.contains(part)) =>
        {
            let raw = process::capture(&mut command(&cluster, &id, false), cancel, TIMEOUT)?;
            return Ok(Controller {
                raw,
                values: BTreeMap::new(),
                updated: update_timestamp(),
            });
        }
        Err(error) => return Err(error),
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid scontrol JSON: {e}"))?;
    if let Some(errors) = value["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())
    {
        return Err(format!("scontrol: {}", Value::Array(errors.clone())));
    }
    let records = value[if id.contains('.') { "steps" } else { "jobs" }]
        .as_array()
        .ok_or("scontrol returned no job/step records")?;
    let record = match records.as_slice() {
        [record] => record,
        [] => return Err("Task is no longer available in scontrol".into()),
        _ => return Err("Multiple task records: use --array or an exact component ID".into()),
    };
    if !record.is_object() {
        return Err("invalid task record".into());
    }
    let values = summary(record);
    Ok(Controller {
        raw: serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?,
        values,
        updated: update_timestamp(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Raw,
    Stdout,
}
impl Tab {
    fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Raw => 1,
            Self::Stdout => 2,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => " [Overview]   Raw   Stdout",
            Self::Raw => " Overview   [Raw]   Stdout",
            Self::Stdout => " Overview   Raw   [Stdout]",
        }
    }
}

pub struct Popup {
    pub row: Row,
    pub snapshot_time: String,
    pub controller: Option<Controller>,
    pub error: Option<String>,
    pub tab: Tab,
    pub stdout: Option<super::stdout::Snapshot>,
    pub stdout_error: Option<String>,
    pub stdout_lines_error: Option<String>,
    pub stdout_loaded: bool,
    pub stdout_wrap: bool,
    pub raw_wrap: bool,
    pub search_pattern: String,
    pub search_edit: Option<String>,
    pub search_anchor: Option<super::stdout::Anchor>,
    pub search_message: Option<String>,
    pub x: usize,
    pub y: usize,
    scroll: [(usize, usize); 3],
}
impl Popup {
    pub fn new(row: Row, snapshot_time: String) -> Self {
        Self {
            row,
            snapshot_time,
            controller: None,
            error: None,
            tab: Tab::Overview,
            stdout: None,
            stdout_error: None,
            stdout_lines_error: None,
            stdout_loaded: false,
            stdout_wrap: false,
            raw_wrap: false,
            search_pattern: String::new(),
            search_edit: None,
            search_anchor: None,
            search_message: None,
            x: 0,
            y: 0,
            scroll: [(0, 0); 3],
        }
    }
    pub fn switch_tab(&mut self, backwards: bool) {
        let tabs = [Tab::Overview, Tab::Raw, Tab::Stdout];
        let index = self.tab.index();
        self.scroll[index] = (self.x, self.y);
        self.tab = tabs[(index + if backwards { 2 } else { 1 }) % tabs.len()];
        (self.x, self.y) = self.scroll[self.tab.index()];
    }
    pub fn wrapped(&self) -> bool {
        match self.tab {
            Tab::Overview => true,
            Tab::Raw => self.raw_wrap,
            Tab::Stdout => self.stdout_wrap,
        }
    }
    pub fn lines(&self) -> Vec<String> {
        if self.tab == Tab::Stdout {
            return if let Some(error) = &self.stdout_error {
                vec![
                    "Cannot load stdout".into(),
                    error.clone(),
                    "Press r to retry".into(),
                ]
            } else {
                vec![]
            };
        }
        let mut lines = Vec::new();
        if self.tab == Tab::Raw {
            lines.push("QUEUE FIELDS · complete raw values".into());
            lines.extend(
                serde_json::to_string_pretty(&self.row)
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned),
            );
            if let Some(controller) = &self.controller {
                lines.push(String::new());
                lines.push("SCONTROL RESPONSE".into());
                lines.extend(controller.raw.lines().map(str::to_owned));
            }
        } else {
            let values: BTreeMap<_, _> = self
                .row
                .iter()
                .map(|(key, value)| (normalized(key), value.clone()))
                .collect();
            for (section, fields) in [
                (
                    "IDENTITY & STATE",
                    vec![
                        ("Name", vec!["jobname", "stepname", "name"]),
                        ("User", vec!["username", "userid", "user"]),
                        (
                            "State",
                            vec!["jobstate", "stepstate", "state", "statecompact"],
                        ),
                        ("Account", vec!["account"]),
                        ("QOS", vec!["qos"]),
                        ("Partition", vec!["partition"]),
                        ("Cluster", vec!["cluster"]),
                        ("Reason", vec!["statereason", "reason", "reasonlist"]),
                    ],
                ),
                (
                    "TIME & SCHEDULING",
                    vec![
                        ("Time used", vec!["timeused"]),
                        ("Time left", vec!["timeleft"]),
                        ("Time limit", vec!["timelimit"]),
                        ("Submitted", vec!["submittime"]),
                        ("Start", vec!["starttime"]),
                        ("End", vec!["endtime"]),
                        ("Priority", vec!["priority"]),
                        ("Dependency", vec!["dependency"]),
                    ],
                ),
                (
                    "RESOURCES & NODES",
                    vec![
                        (
                            "Resources",
                            vec!["tresallocstr", "tresalloc", "tresreqstr", "tres"],
                        ),
                        ("CPUs", vec!["cpus", "numcpus"]),
                        ("Memory", vec!["memorypernode", "minmemory"]),
                        ("Nodes", vec!["nodes", "nodelist", "reasonlist"]),
                        ("Node count", vec!["nodecount", "numnodes"]),
                        ("Batch host", vec!["batchhost"]),
                    ],
                ),
                (
                    "PATHS & COMMAND",
                    vec![
                        ("Command", vec!["command"]),
                        ("Work directory", vec!["currentworkingdirectory", "workdir"]),
                        ("Stdout", vec!["standardoutput", "stdout"]),
                        ("Stderr", vec!["standarderror", "stderr"]),
                    ],
                ),
            ] {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(section.into());
                for (label, aliases) in fields {
                    let controller = self.controller.as_ref().and_then(|controller| {
                        aliases.iter().find_map(|key| controller.values.get(*key))
                    });
                    let value = controller
                        .or_else(|| {
                            aliases
                                .iter()
                                .find_map(|key| values.get(*key).filter(|v| !v.is_empty()))
                        })
                        .map_or("—", String::as_str);
                    lines.push(format!("{label:16} {value}"));
                }
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_step_fields_replace_snapshot_aliases_and_keep_raw_response() {
        let record = serde_json::json!({"id":"42.0","name":"controller-name","state":["COMPLETED"],"number_cpus":8,"time":{"start":1900000000u64,"limit":{"set":true,"infinite":false,"number":90},"running":1234},"tres":{"allocation":"cpu=8,mem=32G"}});
        let mut popup = Popup::new(
            Row::from([
                ("stepname".into(), "snapshot-name".into()),
                ("stepstate".into(), "RUNNING".into()),
                ("timelimit".into(), "0:10:00".into()),
                ("timeused".into(), "0:00:01".into()),
                ("numcpus".into(), "1".into()),
                ("starttime".into(), "old-start".into()),
            ]),
            "snapshot-time".into(),
        );
        let raw = serde_json::to_string_pretty(&record).unwrap();
        popup.controller = Some(Controller {
            raw: raw.clone(),
            values: summary(&record),
            updated: "now".into(),
        });
        let lines = popup.lines().join("\n");
        for expected in [
            "controller-name",
            "COMPLETED",
            "1:30:00",
            "0:20:34",
            "1:09:26",
            "cpu=8,mem=32G",
        ] {
            assert!(lines.contains(expected), "{expected}");
        }
        for stale in [
            "snapshot-name",
            "RUNNING",
            "old-start",
            "0:10:00",
            "0:00:01",
            "allocation",
        ] {
            assert!(!lines.contains(stale), "{stale}");
        }
        assert!(
            lines
                .lines()
                .any(|line| line.starts_with("CPUs") && line.ends_with('8'))
        );
        assert!(lines.contains(&crate::queue::timestamp(1900000000)));
        popup.tab = Tab::Raw;
        assert!(popup.lines().join("\n").contains(&raw));
    }
    #[test]
    fn job_summary_prefers_controller_state_and_represents_unset_values() {
        let record = serde_json::json!({"name":"new-job","job_state":["PENDING"],"time_limit":{"set":true,"number":60},"start_time":{"set":false,"number":0},"tres_alloc_str":"","tres_req_str":"cpu=4,mem=8G","cpus":4,"user_name":"demo"});
        let values = summary(&record);
        assert_eq!(values["name"], "new-job");
        assert_eq!(values["state"], "PENDING");
        assert_eq!(values["starttime"], "—");
        assert_eq!(values["tresallocstr"], "cpu=4,mem=8G");
        assert_eq!(values["timelimit"], "1:00:00");
    }
    #[test]
    fn memory_summary_preserves_units_and_basis_and_skips_unset_alternatives() {
        let unset = serde_json::json!({"set":false,"infinite":false,"number":0});
        let set = serde_json::json!({"set":true,"infinite":false,"number":2048});
        for (node, cpu, expected) in [
            (unset.clone(), set.clone(), "2048 MiB/CPU"),
            (set, unset.clone(), "2048 MiB/node"),
            (unset.clone(), unset.clone(), "—"),
            (
                unset,
                serde_json::json!({"set":false,"infinite":true,"number":0}),
                "UNLIMITED",
            ),
        ] {
            let record = serde_json::json!({"memory_per_node":node,"memory_per_cpu":cpu});
            assert_eq!(summary(&record)["memorypernode"], expected);
        }
    }
    #[test]
    fn controller_limits_and_dates_are_readable_without_changing_raw_data() {
        assert_eq!(
            field_value(
                "time_limit",
                &serde_json::json!({"set":true,"infinite":false,"number":90})
            ),
            "1:30:00"
        );
        assert_eq!(
            field_value(
                "time_limit",
                &serde_json::json!({"set":true,"infinite":true,"number":0})
            ),
            "UNLIMITED"
        );
        assert_eq!(
            field_value("end_time", &serde_json::json!({"set":true,"number":0})),
            "—"
        );
    }
    #[test]
    fn overview_and_raw_keep_complete_resource_values() {
        let popup = Popup::new(
            Row::from([
                ("jobid".into(), "42".into()),
                ("tres-alloc".into(), "cpu=8,billing=8".into()),
            ]),
            "now".into(),
        );
        assert!(popup.lines().join("\n").contains("billing=8"));
        let mut popup = popup;
        popup.tab = Tab::Raw;
        assert!(popup.lines().join("\n").contains("billing=8"));
    }
    #[test]
    fn array_groups_never_fetch_an_arbitrary_task() {
        let row = Row::from([("jobid".into(), "42_[1-10]".into())]);
        assert!(
            fetch(&row, &AtomicBool::new(false))
                .unwrap_err()
                .contains("--array")
        );
    }
}
