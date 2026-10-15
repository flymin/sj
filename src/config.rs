use crate::theme::{Style, Theme};
use crate::util::{self, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const TEMPLATE: &str = include_str!("../config/default.toml");

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Right,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Overflow {
    Truncate,
    #[default]
    Ellipsis,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Auto,
    Raw,
    Duration,
    Timestamp,
    Reason,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Column {
    pub title: Option<String>,
    pub format: Format,
    pub align: Align,
    pub min_width: usize,
    pub preferred_width: Option<usize>,
    pub max_width: Option<usize>,
    pub overflow: Overflow,
    pub optional: bool,
    pub shrink_priority: i32,
    pub hide_priority: i32,
    pub grow_priority: i32,
    pub style: Style,
}
impl Default for Column {
    fn default() -> Self {
        Self {
            title: None,
            format: Format::Auto,
            align: Align::Left,
            min_width: 1,
            preferred_width: None,
            max_width: None,
            overflow: Overflow::Ellipsis,
            optional: false,
            shrink_priority: 20,
            hide_priority: 70,
            grow_priority: 10,
            style: Style::default(),
        }
    }
}
impl Column {
    pub fn title<'a>(&'a self, field: &'a str) -> &'a str {
        self.title.as_deref().unwrap_or(field)
    }
    fn validate(&self) -> Result<()> {
        if self.min_width == 0 || self.max_width == Some(0) || self.preferred_width == Some(0) {
            return Err("column widths must be positive".into());
        }
        if self.max_width.is_some_and(|max| max < self.min_width) {
            return Err("max_width must be >= min_width".into());
        }
        if self
            .title
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.chars().any(char::is_control))
        {
            return Err("title must be nonempty and contain no control characters".into());
        }
        self.style.validate()
    }
    fn patched(&self, patch: &toml::Table) -> Result<Self> {
        let mut value = toml::Value::try_from(self).map_err(|e| e.to_string())?;
        merge(&mut value, toml::Value::Table(patch.clone()));
        let column: Self = value.try_into().map_err(|e| e.to_string())?;
        Ok(column)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    pub min_width: Option<usize>,
    pub max_width: Option<usize>,
    pub order: Option<Vec<String>>,
    pub columns: BTreeMap<String, toml::Table>,
    pub theme: Theme,
}
impl Rule {
    fn matches(&self, width: usize) -> bool {
        self.min_width.is_none_or(|n| width >= n) && self.max_width.is_none_or(|n| width <= n)
    }
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    pub order: Vec<String>,
    pub filter: Option<crate::filter::Expression>,
    pub columns: BTreeMap<String, toml::Table>,
    pub theme: Theme,
    pub rules: Vec<Rule>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub default_view: String,
    pub start_view: String,
    pub steps_view: String,
    pub columns: BTreeMap<String, Column>,
    pub views: BTreeMap<String, View>,
    pub theme: Theme,
}
pub struct Resolved {
    pub columns: Vec<(String, Column)>,
    pub theme: Theme,
}

fn merge(base: &mut toml::Value, patch: toml::Value) {
    match (base, patch) {
        (toml::Value::Table(base), toml::Value::Table(patch)) => {
            for (key, value) in patch {
                if let Some(old) = base.get_mut(&key) {
                    merge(old, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, patch) => *base = patch,
    }
}
fn normalize_columns(value: &mut toml::Value) -> Result<()> {
    fn table(value: Option<&mut toml::Value>) -> Result<()> {
        if let Some(toml::Value::Table(columns)) = value {
            let old = std::mem::take(columns);
            for (name, value) in old {
                if columns.insert(name.to_ascii_lowercase(), value).is_some() {
                    return Err(format!(
                        "columns: duplicate field {name:?} (names are case-insensitive)"
                    ));
                }
            }
        }
        Ok(())
    }
    table(value.get_mut("columns"))?;
    if let Some(toml::Value::Table(views)) = value.get_mut("views") {
        for (_, view) in views.iter_mut() {
            table(view.get_mut("columns"))?;
            if let Some(toml::Value::Array(rules)) = view.get_mut("rules") {
                for rule in rules {
                    table(rule.get_mut("columns"))?;
                }
            }
        }
    }
    Ok(())
}
fn unique(names: impl IntoIterator<Item = String>, context: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for name in names {
        if name.is_empty() || !seen.insert(name.to_ascii_lowercase()) {
            return Err(format!(
                "{context}: empty or duplicate field {name:?} (names are case-insensitive)"
            ));
        }
    }
    Ok(())
}
impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let mut value: toml::Value = toml::from_str(TEMPLATE).expect("embedded config");
        let mut patch: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        normalize_columns(&mut value)?;
        normalize_columns(&mut patch)?;
        merge(&mut value, patch);
        let config: Self = value.try_into().map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
    pub fn builtin() -> Self {
        Self::parse("").expect("valid built-in config")
    }
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported schema_version {}; expected 1",
                self.schema_version
            ));
        }
        for (key, name) in [
            ("default_view", &self.default_view),
            ("start_view", &self.start_view),
            ("steps_view", &self.steps_view),
        ] {
            if !self.views.contains_key(name) {
                return Err(format!("{key}: unknown view {name:?}"));
            }
        }
        unique(self.columns.keys().cloned(), "columns")?;
        for (name, column) in &self.columns {
            column
                .validate()
                .map_err(|e| format!("columns.{name}: {e}"))?;
        }
        self.theme.validate().map_err(|e| format!("theme: {e}"))?;
        for (name, view) in &self.views {
            let key = format!("views.{name}");
            if view.order.is_empty() {
                return Err(format!("{key}.order must contain at least one field"));
            }
            unique(view.order.clone(), &format!("{key}.order"))?;
            unique(view.columns.keys().cloned(), &format!("{key}.columns"))?;
            view.theme
                .validate()
                .map_err(|e| format!("{key}.theme: {e}"))?;
            let base = self.view_columns(view)?;
            for (field, column) in &base {
                column
                    .validate()
                    .map_err(|e| format!("{key}.columns.{field}: {e}"))?;
            }
            let mut boundaries = BTreeSet::from([0]);
            for (index, rule) in view.rules.iter().enumerate() {
                let key = format!("{key}.rules[{index}]");
                if rule
                    .min_width
                    .zip(rule.max_width)
                    .is_some_and(|(min, max)| min > max)
                {
                    return Err(format!("{key}: min_width must be <= max_width"));
                }
                if let Some(order) = &rule.order {
                    if order.is_empty() {
                        return Err(format!("{key}.order must contain at least one field"));
                    }
                    unique(order.clone(), &format!("{key}.order"))?;
                }
                unique(rule.columns.keys().cloned(), &format!("{key}.columns"))?;
                rule.theme
                    .validate()
                    .map_err(|e| format!("{key}.theme: {e}"))?;
                // Parse every patch, then validate every distinct combination of
                // matching rules at its boundary, including where a rule expires.
                let mut columns = base.clone();
                patch_columns(&mut columns, &rule.columns).map_err(|e| format!("{key}: {e}"))?;
                boundaries.insert(rule.min_width.unwrap_or(0));
                if let Some(after) = rule.max_width.and_then(|n| n.checked_add(1)) {
                    boundaries.insert(after);
                }
            }
            for width in boundaries {
                self.resolve(name, width)
                    .map_err(|e| format!("{key} at width {width}: {e}"))?;
            }
        }
        Ok(())
    }
    fn view_columns(&self, view: &View) -> Result<BTreeMap<String, Column>> {
        let mut columns = self
            .columns
            .iter()
            .map(|(name, column)| (name.to_ascii_lowercase(), column.clone()))
            .collect();
        patch_columns(&mut columns, &view.columns)?;
        Ok(columns)
    }
    pub fn resolve(&self, name: &str, width: usize) -> Result<Resolved> {
        let view = self
            .views
            .get(name)
            .ok_or_else(|| format!("unknown view {name:?}; use sj config views"))?;
        let mut order = view.order.clone();
        let mut columns = self.view_columns(view)?;
        let mut theme = self.theme.clone();
        theme.overlay(&view.theme);
        for rule in &view.rules {
            if rule.matches(width) {
                if let Some(next) = &rule.order {
                    order.clone_from(next);
                }
                patch_columns(&mut columns, &rule.columns)?;
                theme.overlay(&rule.theme);
            }
        }
        for (field, column) in &columns {
            column
                .validate()
                .map_err(|e| format!("columns.{field}: {e}"))?;
        }
        Ok(Resolved {
            columns: order
                .into_iter()
                .map(|name| {
                    let column = columns
                        .get(&name.to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_default();
                    (name, column)
                })
                .collect(),
            theme,
        })
    }
    pub fn fields(&self, name: &str) -> Result<Vec<String>> {
        let view = self
            .views
            .get(name)
            .ok_or_else(|| format!("unknown view {name:?}; use sj config views"))?;
        let mut fields = BTreeSet::new();
        fields.extend(view.order.iter().cloned());
        fields.extend(view.columns.keys().cloned());
        let mut state = !self.theme.states.is_empty() || !view.theme.states.is_empty();
        for rule in &view.rules {
            if let Some(order) = &rule.order {
                fields.extend(order.iter().cloned());
            }
            fields.extend(rule.columns.keys().cloned());
            state |= !rule.theme.states.is_empty();
        }
        // The caller adds the appropriate full-state field for job/step mode.
        if state {
            fields.insert("__state".into());
        }
        Ok(fields.into_iter().collect())
    }
}
fn patch_columns(
    columns: &mut BTreeMap<String, Column>,
    patches: &BTreeMap<String, toml::Table>,
) -> Result<()> {
    for (name, patch) in patches {
        let key = name.to_ascii_lowercase();
        let column = columns
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .patched(patch)
            .map_err(|e| format!("columns.{name}: {e}"))?;
        columns.insert(key, column);
    }
    Ok(())
}

pub fn default_path_from(
    xdg: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<PathBuf> {
    if let Some(xdg) = xdg.filter(|p| !p.is_empty() && Path::new(p).is_absolute()) {
        return Ok(Path::new(xdg).join("sj/config.toml"));
    }
    home.filter(|p| !p.is_empty())
        .map(|p| Path::new(p).join(".config/sj/config.toml"))
        .ok_or_else(|| "HOME is unset; provide --config FILE".into())
}
pub fn default_path() -> Result<PathBuf> {
    default_path_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

// Write a complete file first, then atomically link it without replacing a winner.
fn create_default(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("config has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".config-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(TEMPLATE.as_bytes())?;
        file.sync_all()?;
        match std::fs::hard_link(&temporary, path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        }
    })();
    let _ = std::fs::remove_file(temporary);
    result
}
pub struct Loaded {
    pub config: Config,
    pub path: Option<PathBuf>,
}
pub fn load(explicit: Option<&Path>, create: bool) -> Result<Loaded> {
    let path = match explicit
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(default_path)
    {
        Ok(path) => path,
        Err(e) if create => {
            eprintln!("sj: warning: {e}; using built-in configuration");
            return Ok(Loaded {
                config: Config::builtin(),
                path: None,
            });
        }
        Err(e) => return Err(e),
    };
    if create
        && explicit.is_none()
        && !path.exists()
        && let Err(e) = create_default(&path)
    {
        eprintln!(
            "sj: warning: cannot create {}: {e}; using built-in configuration",
            path.display()
        );
        return Ok(Loaded {
            config: Config::builtin(),
            path: None,
        });
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if explicit.is_none() && !create && e.kind() == std::io::ErrorKind::NotFound => {
            String::new()
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let config = Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Loaded {
        config,
        path: Some(path),
    })
}

pub fn run(args: Vec<String>) -> Result<i32> {
    if crate::help::show_if_requested(&args, crate::help::CONFIG)? {
        return Ok(0);
    }
    let mut action = None;
    let mut path = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--config" || arg.starts_with("--config=") {
            let value = if let Some((_, v)) = arg.split_once('=') {
                v.to_owned()
            } else {
                i += 1;
                args.get(i).ok_or("--config requires a path")?.clone()
            };
            path = Some(PathBuf::from(value));
        } else if matches!(arg.as_str(), "path" | "views" | "check") && action.is_none() {
            action = Some(arg.as_str());
        } else {
            return Err(format!("unknown config argument {arg:?}; use sj config -h"));
        }
        i += 1;
    }
    match action {
        Some("path") => util::print(&format!(
            "{}\n",
            path.map(Ok).unwrap_or_else(default_path)?.display()
        ))?,
        Some(action) => {
            let loaded = load(path.as_deref(), false)?;
            if action == "views" {
                for name in loaded.config.views.keys() {
                    util::print(&format!("{name}\n"))?;
                }
            } else {
                crate::fields::check(&loaded.config).map_err(|e| {
                    format!(
                        "{}: {e}",
                        loaded.path.as_ref().map_or_else(
                            || "built-in configuration".into(),
                            |p| p.display().to_string()
                        )
                    )
                })?;
                util::print("Configuration OK\n")?;
            }
        }
        None => {
            util::print(crate::help::CONFIG)?;
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_follow_xdg() {
        for xdg in [None, Some(""), Some("relative")] {
            assert_eq!(
                default_path_from(
                    xdg.map(std::ffi::OsStr::new),
                    Some(std::ffi::OsStr::new("/home/example"))
                )
                .unwrap(),
                PathBuf::from("/home/example/.config/sj/config.toml")
            );
        }
        assert_eq!(
            default_path_from(Some(std::ffi::OsStr::new("/config")), None).unwrap(),
            PathBuf::from("/config/sj/config.toml")
        );
    }
    #[test]
    fn patches_and_rules_start_from_clean_base() {
        let c = Config::parse("[views.monitor]\norder=['Name']\n[views.monitor.columns.Name]\nmin_width=3\n[[views.monitor.rules]]\nmin_width=100\norder=['JobId','Name']\n[views.monitor.rules.columns.Name]\nmax_width=60\n[[views.monitor.rules]]\nmin_width=120\n[views.monitor.rules.columns.Name]\nmax_width=80").unwrap();
        assert_eq!(
            c.resolve("monitor", 130).unwrap().columns[1].1.max_width,
            Some(80)
        );
        assert_eq!(
            c.resolve("monitor", 110).unwrap().columns[1].1.max_width,
            Some(60)
        );
        assert_eq!(c.resolve("monitor", 80).unwrap().columns.len(), 1);
    }
    #[test]
    fn rejects_bad_schema_and_columns() {
        for patch in [
            "schema_version=2",
            "unknown=true",
            "default_view='missing'",
            "[columns.Name]\nmin_width=0",
            "[columns.Name]\nmax_width=2",
            "[columns.Name]\nalign='center'",
            "[columns.Name]\nunknown=1",
            "[columns.Name]\nmin_width=1\n[columns.name]\nmin_width=2",
            "[views.queue]\norder=['JobId','jobid']",
            "[[views.queue.rules]]\nmin_width=100\nmax_width=20",
            "[theme.header]\nfg='orange'",
            "[theme.header]\nfg=256",
        ] {
            assert!(Config::parse(patch).is_err(), "{patch}");
        }
    }
    #[test]
    fn field_case_is_normalized_before_merging() {
        let c =
            Config::parse("[columns.jobid]\ntitle='ID'\n[views.compact]\norder=['JOBID']").unwrap();
        let view = c.resolve("compact", 80).unwrap();
        assert_eq!(view.columns[0].1.title.as_deref(), Some("ID"));
        assert_eq!(view.columns[0].1.min_width, 7);
    }
    #[test]
    fn rules_validate_expiration_and_later_overrides() {
        let good = "[views.test]\norder=['Account']\n[[views.test.rules]]\nmax_width=100\n[views.test.rules.columns.Account]\nmin_width=20\n[[views.test.rules]]\nmax_width=100\n[views.test.rules.columns.Account]\nmax_width=30";
        assert!(Config::parse(good).is_ok());
        let bad = "[views.test]\norder=['Account']\n[views.test.columns.Account]\nmin_width=20\n[[views.test.rules]]\nmax_width=100\n[views.test.rules.columns.Account]\nmin_width=1\n[[views.test.rules]]\n[views.test.rules.columns.Account]\nmax_width=10";
        assert!(Config::parse(bad).unwrap_err().contains("width"));
        // A preferred width is a preference, clamped after a narrower cap.
        assert!(Config::parse("[columns.Name]\nmax_width=20").is_ok());
    }
}
