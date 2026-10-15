use crate::config::Config;
use crate::util::{self, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

pub const SEP: char = '\u{1f}';
pub const RECORD: char = '\u{1e}';
pub type Row = BTreeMap<String, String>;

pub struct Catalog {
    pub names: BTreeMap<String, String>,
    pub steps: bool,
    pub legacy: bool,
}
impl Catalog {
    pub fn fetch(steps: bool) -> Result<Self> {
        let mut command = Command::new("squeue");
        if steps {
            command.arg("--steps");
        }
        command.arg("--helpFormat").env("LC_ALL", "C");
        match util::capture(&mut command) {
            Ok(text) => Self::parse(&text, steps),
            Err(error)
                if error.contains("unrecognized option '--helpFormat'")
                    || error.contains("unrecognized option `--helpFormat'") =>
            {
                let mut catalog = Self::parse(
                    if steps {
                        crate::legacy_fields::STEPS
                    } else {
                        crate::legacy_fields::JOBS
                    },
                    steps,
                )?;
                catalog.legacy = true;
                Ok(catalog)
            }
            Err(error) => Err(error),
        }
    }
    pub fn parse(text: &str, steps: bool) -> Result<Self> {
        let names: BTreeMap<_, _> = text
            .split_whitespace()
            .filter(|name| {
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
            .map(|name| (name.to_ascii_lowercase(), name.into()))
            .collect();
        if !names.contains_key("jobid") {
            return Err("squeue --helpFormat returned no field catalog".into());
        }
        Ok(Self {
            names,
            steps,
            legacy: false,
        })
    }
    pub fn canonical(&self, name: &str) -> Result<String> {
        self.names.get(&name.to_ascii_lowercase()).cloned().ok_or_else(|| {
            if self.legacy { format!("field {name:?} is not in the legacy {} compatibility catalog; use explicit native output for version-specific fields", if self.steps { "step" } else { "job" }) }
            else { format!("unknown {} field {name:?}; use sj q {}--list-fields (full field names required)", if self.steps { "step" } else { "job" }, if self.steps { "--steps " } else { "" }) }
        })
    }
    pub fn for_view(&self, config: &Config, name: &str) -> Result<Vec<String>> {
        let mut names = BTreeSet::new();
        for field in config.fields(name)? {
            let field = if field == "__state" {
                if self.steps { "StepState" } else { "State" }
            } else {
                &field
            };
            names.insert(self.canonical(field)?);
        }
        Ok(names.into_iter().collect())
    }
}
pub fn format(names: &[String]) -> String {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            format!(
                "{name}:0{}",
                if index + 1 == names.len() {
                    RECORD
                } else {
                    SEP
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}
pub fn decode(text: &str, names: &[String]) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    for (index, record) in text.split(RECORD).enumerate() {
        // squeue adds a newline after each record suffix; field newlines stay data.
        let record = if index == 0 {
            record
        } else {
            record.strip_prefix('\n').unwrap_or(record)
        };
        if record.is_empty() {
            continue;
        }
        let values: Vec<_> = record.split(SEP).collect();
        if values.len() != names.len() {
            return Err(
                "unexpected squeue record; use --json or an explicit --format for native output"
                    .into(),
            );
        }
        rows.push(
            names
                .iter()
                .zip(values)
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_owned()))
                .collect(),
        );
    }
    Ok(rows)
}
pub fn clean_rows(rows: &mut [Row]) {
    for row in rows {
        for value in row.values_mut() {
            *value = util::clean(value.trim());
        }
    }
}
pub fn check(config: &Config) -> Result<()> {
    let jobs = Catalog::fetch(false)?;
    let steps = Catalog::fetch(true)?;
    for field in config.columns.keys() {
        if jobs.canonical(field).is_err() && steps.canonical(field).is_err() {
            return Err(format!("columns.{field}: unknown Slurm field"));
        }
    }
    let validate = |name: &str, catalog: &Catalog| -> Result<()> {
        catalog.for_view(config, name)?;
        if let Some(filter) = &config.views[name].filter {
            filter
                .validate(catalog)
                .map_err(|e| format!("filter: {e}"))?;
        }
        Ok(())
    };
    for (selector, name, catalog) in [
        ("default_view", &config.default_view, &jobs),
        ("start_view", &config.start_view, &jobs),
        ("steps_view", &config.steps_view, &steps),
    ] {
        validate(name, catalog).map_err(|e| format!("{selector} -> views.{name}: {e}"))?;
    }
    for name in config.views.keys() {
        if let Err(job_error) = validate(name, &jobs) {
            validate(name, &steps)
                .map_err(|step_error| format!("views.{name}: {job_error}; {step_error}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_catalogs_include_shared_job_and_step_fields() {
        let jobs = Catalog::parse(crate::legacy_fields::JOBS, false).unwrap();
        let steps = Catalog::parse(crate::legacy_fields::STEPS, true).unwrap();
        assert_eq!(jobs.names.len(), 118);
        assert_eq!(steps.names.len(), 30);
        assert_eq!(jobs.canonical("NumTasks").unwrap(), "numtasks");
        assert_eq!(steps.canonical("NumTasks").unwrap(), "numtasks");
        assert_eq!(steps.canonical("Container").unwrap(), "container");
    }
    #[test]
    fn exact_names_and_multiline_records() {
        let catalog = Catalog::parse("JobId Name CPUsPerTask", false).unwrap();
        assert_eq!(catalog.canonical("jobid").unwrap(), "JobId");
        assert!(catalog.canonical("CPUs").is_err());
        let names = vec!["JobId".into(), "Name".into()];
        assert_eq!(format(&names), "JobId:0\x1f,Name:0\x1e");
        let mut rows = decode("42\x1fa\nb\x1e\n43\x1fc\x1e\n", &names).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "a\nb");
        clean_rows(&mut rows);
        assert_eq!(rows[0]["name"], "a b");
        assert!(decode("42\x1fa\x1fextra\x1e\n", &names).is_err());
    }
    #[test]
    fn leading_newlines_and_spaces_are_data_until_display_cleanup() {
        let mut rows = decode("\n a \x1e\n\nb \x1e\n", &["Name".into()]).unwrap();
        assert_eq!(rows[0]["name"], "\n a ");
        assert_eq!(rows[1]["name"], "\nb ");
        clean_rows(&mut rows);
        assert_eq!(rows[0]["name"], "a");
        assert_eq!(rows[1]["name"], "b");
    }
}
