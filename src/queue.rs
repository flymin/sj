use crate::config;
use crate::fields::{self, Catalog};
use crate::filter::Expression;
use crate::layout;
use crate::theme::ColorMode;
use crate::util::{self, Result};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

pub use crate::layout::clip;
static SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::Relaxed);
}

#[derive(Debug, Default)]
struct Options {
    native: Vec<String>,
    interval: Option<Duration>,
    width: Option<usize>,
    start: bool,
    steps: bool,
    noheader: bool,
    passthrough: bool,
    config: Option<PathBuf>,
    view: Option<String>,
    color: ColorMode,
    list_fields: bool,
    filters: Vec<Expression>,
    no_view_filter: bool,
    start_indices: Vec<usize>,
    states_option: bool,
    sort_option: bool,
    bundled_iterate: bool,
}

#[derive(Default)]
struct NativeOption {
    takes_value: bool,
    start: bool,
    steps: bool,
    states: bool,
    sort: bool,
    noheader: bool,
    passthrough: bool,
    help: bool,
    iterate: bool,
}
fn native_option(arg: &str) -> NativeOption {
    let mut info = NativeOption::default();
    if arg.starts_with("--") {
        let key = arg.split('=').next().unwrap_or(arg);
        const REQUIRED: &[&str] = &[
            "--accounts",
            "--account",
            "--name",
            "--job-name",
            "--licenses",
            "--cluster",
            "--clusters",
            "--node",
            "--nodes",
            "--nodelist",
            "--partition",
            "--partitions",
            "--qos",
            "--reservation",
            "--sort",
            "--states",
            "--user",
            "--users",
            "--format",
            "--Format",
            "--autocomplete",
            "--iterate",
            "--running-over",
            "--running-under",
        ];
        const FLAGS: &[&str] = &[
            "--all",
            "--array",
            "--expand-patterns",
            "--federation",
            "--help",
            "--hide",
            "--job",
            "--jobs",
            "--local",
            "--long",
            "--me",
            "--noconvert",
            "--noheader",
            "--notme",
            "--only-job-state",
            "--priority",
            "--sib",
            "--sibling",
            "--start",
            "--step",
            "--steps",
            "--usage",
            "--verbose",
            "--version",
            "--json",
            "--yaml",
            "--helpformat",
            "--helpFormat",
            "--helpstate",
        ];
        fn semantic(name: &str) -> &str {
            match name {
                "--account" | "--accounts" => "--accounts",
                "--cluster" | "--clusters" => "--clusters",
                "--node" | "--nodes" | "--nodelist" => "--nodelist",
                "--partition" | "--partitions" => "--partitions",
                "--user" | "--users" => "--users",
                "--step" | "--steps" => "--steps",
                "--job" | "--jobs" => "--jobs",
                "--sib" | "--sibling" => "--sibling",
                _ => name,
            }
        }
        let mut choices = REQUIRED
            .iter()
            .chain(FLAGS)
            .copied()
            .filter(|name| name.starts_with(key))
            .map(semantic);
        let canonical = if REQUIRED.contains(&key) || FLAGS.contains(&key) {
            Some(semantic(key))
        } else {
            let first = choices.next();
            if choices.all(|name| Some(name) == first) {
                first
            } else {
                None
            }
        };
        if let Some(key) = canonical {
            info.takes_value = REQUIRED.contains(&key) && !arg.contains('=');
            info.start = key == "--start";
            info.steps = matches!(key, "--step" | "--steps");
            info.states = key == "--states";
            info.sort = key == "--sort";
            info.noheader = key == "--noheader";
            info.help = key == "--help";
            info.iterate = key == "--iterate";
            info.passthrough = matches!(
                key,
                "--format"
                    | "--Format"
                    | "--long"
                    | "--json"
                    | "--yaml"
                    | "--only-job-state"
                    | "--helpformat"
                    | "--helpFormat"
                    | "--helpstate"
                    | "--usage"
                    | "--version"
            );
        }
    } else if arg.starts_with('-') {
        for (offset, flag) in arg.char_indices().skip(1) {
            info.steps |= flag == 's';
            info.states |= flag == 't';
            info.sort |= flag == 'S';
            info.help |= flag == 'h';
            info.iterate |= flag == 'i';
            info.passthrough |= matches!(flag, 'o' | 'O' | 'l' | 'V');
            if "AiJLnMOopqRStuUw".contains(flag) {
                info.takes_value = offset + flag.len_utf8() == arg.len();
                break;
            }
            // Native optional arguments consume the rest of the same token.
            if matches!(flag, 'j' | 's') {
                break;
            }
        }
    }
    info
}
fn help_requested(args: &[String]) -> bool {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let info = native_option(arg);
        if arg == "-h" || arg == "--help" || info.help {
            return true;
        }
        let owned_value = matches!(
            arg.as_str(),
            "-i" | "--iterate" | "--width" | "--config" | "--view" | "--color" | "--filter"
        );
        i += if owned_value || info.takes_value {
            2
        } else {
            1
        };
    }
    false
}
fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let key = a.split('=').next().unwrap_or(a);
        let iterate = matches!(key, "-i" | "--iterate")
            || (a.starts_with("-i") && a.len() > 2 && !a.starts_with("--"));
        if iterate
            || matches!(
                key,
                "--width" | "--config" | "--view" | "--color" | "--filter"
            )
        {
            let value = if let Some((_, v)) = a.split_once('=') {
                v.to_owned()
            } else if iterate && a.starts_with("-i") && a.len() > 2 {
                a[2..].to_owned()
            } else {
                i += 1;
                args.get(i)
                    .ok_or_else(|| format!("{a} requires a value"))?
                    .clone()
            };
            if value.is_empty() {
                return Err(format!("{key} requires a nonempty value"));
            }
            match key {
                "--config" => o.config = Some(value.into()),
                "--view" => o.view = Some(value),
                "--color" => o.color = ColorMode::parse(&value)?,
                "--filter" => o
                    .filters
                    .push(Expression::parse(&value).map_err(|e| format!("--filter: {e}"))?),
                _ => {
                    let n: usize = value
                        .parse()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| format!("{a} requires a positive integer"))?;
                    if iterate {
                        o.interval = Some(Duration::from_secs(n as u64));
                    } else {
                        o.width = Some(n);
                    }
                }
            }
        } else if a == "--list-fields" {
            o.list_fields = true;
        } else if a == "--no-view-filter" {
            o.no_view_filter = true;
        } else {
            let info = native_option(a);
            if info.start {
                o.start_indices.push(o.native.len());
            }
            o.start |= info.start;
            o.steps |= info.steps;
            o.states_option |= info.states;
            o.sort_option |= info.sort;
            o.noheader |= info.noheader;
            o.passthrough |= info.passthrough;
            o.bundled_iterate |= info.iterate;
            o.native.push(args[i].clone());
            if info.takes_value {
                i += 1;
                o.native.push(
                    args.get(i)
                        .ok_or_else(|| format!("{a} requires a value"))?
                        .clone(),
                );
            }
        }
        i += 1;
    }
    if o.passthrough && o.view.is_some() {
        return Err("--view cannot be combined with native output options".into());
    }
    if o.list_fields && (o.passthrough || o.view.is_some()) {
        return Err("--list-fields cannot be combined with native output or --view".into());
    }
    if (!o.filters.is_empty() || o.no_view_filter) && (o.passthrough || o.list_fields) {
        return Err("--filter and --no-view-filter cannot be combined with native output options or --list-fields".into());
    }
    if o.bundled_iterate && !o.passthrough {
        return Err("use -i SECONDS or --iterate SECONDS as a separate sj option".into());
    }
    Ok(o)
}
pub fn terminal_width() -> usize {
    crate::terminal::size().0
}

fn adaptive_args(options: &Options, state_env: bool, sort_env: bool) -> Vec<String> {
    // Slurm's --start installs a percent-format default even with --Format set.
    // Its other effects are defaults for states and sort; apply those directly.
    let mut args: Vec<_> = options
        .native
        .iter()
        .enumerate()
        .filter(|(index, _)| !options.start_indices.contains(index))
        .map(|(_, arg)| arg.clone())
        .collect();
    if options.start && !options.steps {
        if !state_env && !options.states_option {
            args.extend(["--states".into(), "PD".into()]);
        }
        if !sort_env && !options.sort_option {
            args.extend(["--sort".into(), "S".into()]);
        }
    }
    args
}
fn update_timestamp() -> String {
    let seconds = unsafe { libc::time(std::ptr::null_mut()) };
    let mut calendar: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_r writes only to this initialized tm, keeping TZ handling in libc.
    if unsafe { libc::localtime_r(&seconds, &mut calendar) }.is_null() {
        return "unknown".into();
    }
    let mut buffer = [0u8; 64];
    let length = unsafe {
        libc::strftime(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c"%Y-%m-%d %H:%M:%S %z".as_ptr(),
            &calendar,
        )
    };
    if length == 0 {
        "unknown".into()
    } else {
        String::from_utf8_lossy(&buffer[..length]).into_owned()
    }
}

fn refresh_header(interval: Duration, updated: &str, width: usize) -> String {
    let width = width.max(1);
    let frequency = format!("Every {}s", interval.as_secs());
    let full = format!("Updated: {updated}");
    let clock = updated.split_whitespace().nth(1).unwrap_or(updated);
    let compact = format!("Updated: {clock}");
    let used = UnicodeWidthStr::width(frequency.as_str());
    for stamp in [&full, &compact] {
        let demand = used + 1 + UnicodeWidthStr::width(stamp.as_str());
        if demand <= width {
            let gap = width - used - UnicodeWidthStr::width(stamp.as_str());
            return format!("{frequency}{}{stamp}\n\n", " ".repeat(gap));
        }
    }
    let stamp = [full.as_str(), compact.as_str(), clock]
        .into_iter()
        .find(|value| UnicodeWidthStr::width(*value) <= width)
        .unwrap_or(clock);
    format!("{}\n{}\n\n", clip(&frequency, width), clip(stamp, width))
}

pub fn run(args: Vec<String>) -> Result<i32> {
    if help_requested(&args) {
        util::print(crate::help::QUEUE)?;
        return Ok(0);
    }
    let options = parse(&args)?;
    if options.passthrough {
        let mut command = Command::new("squeue");
        command.args(&options.native);
        if let Some(interval) = options.interval {
            command.arg(format!("--iterate={}", interval.as_secs()));
        }
        return util::exec(&mut command);
    }
    if options.list_fields {
        let catalog = Catalog::fetch(options.steps)?;
        if catalog.legacy {
            eprintln!(
                "sj: listing the compatibility catalog shared by Slurm 22.05/23.11; version-specific fields may also be available through native output"
            );
        }
        for name in catalog.names.values() {
            util::print(&format!("{name}\n"))?;
        }
        return Ok(0);
    }
    let loaded = config::load(options.config.as_deref(), true)?;
    let config = &loaded.config;
    let view = options.view.as_deref().unwrap_or(if options.steps {
        &config.steps_view
    } else if options.start {
        &config.start_view
    } else {
        &config.default_view
    });
    let context = |error: String| {
        format!(
            "{}: {error}",
            loaded.path.as_ref().map_or_else(
                || "built-in configuration".into(),
                |p| p.display().to_string()
            )
        )
    };
    let catalog = Catalog::fetch(options.steps)?;
    let other_catalog = Catalog::fetch(!options.steps)?;
    for field in config.columns.keys() {
        if catalog.canonical(field).is_err() && other_catalog.canonical(field).is_err() {
            return Err(context(format!("columns.{field}: unknown Slurm field")));
        }
    }
    let mut names: std::collections::BTreeSet<_> = catalog
        .for_view(config, view)
        .map_err(context)?
        .into_iter()
        .collect();
    let view_filter = if options.no_view_filter {
        None
    } else {
        config.views[view].filter.as_ref()
    };
    if let Some(filter) = view_filter {
        names.extend(
            filter
                .validate(&catalog)
                .map_err(|e| context(format!("views.{view}.filter: {e}")))?,
        );
    }
    for filter in &options.filters {
        names.extend(
            filter
                .validate(&catalog)
                .map_err(|e| format!("--filter: {e}"))?,
        );
    }
    let names: Vec<_> = names.into_iter().collect();
    let format = fields::format(&names);
    let native = adaptive_args(
        &options,
        std::env::var_os("SQUEUE_STATES").is_some(),
        std::env::var_os("SQUEUE_SORT").is_some(),
    );
    let tty = io::stdout().is_terminal();
    let color = options.color.enabled(tty);
    SIGNAL.store(0, Ordering::Relaxed);
    if options.interval.is_some() {
        unsafe {
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
    }
    loop {
        let mut command = Command::new("squeue");
        command
            .args(&native)
            .args(["--noheader", "--Format", &format]);
        command.env("SLURM_BITSTR_LEN", "1048576");
        let text = util::capture(&mut command);
        let signal = SIGNAL.load(Ordering::Relaxed);
        if signal != 0 {
            return Ok(128 + signal);
        }
        let mut rows = fields::decode(&text?, &names)?;
        rows.retain(|row| {
            view_filter.is_none_or(|filter| filter.matches(row))
                && options.filters.iter().all(|filter| filter.matches(row))
        });
        fields::clean_rows(&mut rows);
        let width = options.width.unwrap_or_else(terminal_width);
        let resolved = config.resolve(view, width).map_err(context)?;
        let table = layout::render(&rows, &resolved, width, options.noheader, color);
        if let Some(interval) = options.interval.filter(|_| tty) {
            let header = refresh_header(interval, &update_timestamp(), width);
            let header: String = header
                .lines()
                .map(|line| format!("{}\n", resolved.theme.status.paint(line, color)))
                .collect();
            util::print(&format!("\x1b[H\x1b[2J{header}{table}"))?;
        } else {
            util::print(&table)?;
        }
        let Some(interval) = options.interval else {
            return Ok(0);
        };
        let deadline = Instant::now()
            .checked_add(interval)
            .ok_or("iteration interval is too large")?;
        while Instant::now() < deadline {
            let signal = SIGNAL.load(Ordering::Relaxed);
            if signal != 0 {
                return Ok(128 + signal);
            }
            std::thread::sleep(
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_iteration_and_view_without_eating_filters() {
        let args = [
            "--me",
            "--start",
            "-i5",
            "-t",
            "PD",
            "--width=80",
            "--view=compact",
            "--config",
            "./sj.toml",
        ]
        .map(str::to_owned);
        let o = parse(&args).unwrap();
        assert_eq!(o.native, ["--me", "--start", "-t", "PD"]);
        assert_eq!(o.interval, Some(Duration::from_secs(5)));
        assert_eq!(o.width, Some(80));
        assert_eq!(o.view.as_deref(), Some("compact"));
        assert_eq!(o.config, Some(PathBuf::from("./sj.toml")));
        assert!(!parse(&["--steps=42".into()]).unwrap().passthrough);
        assert!(parse(&["--view=compact".into(), "--json".into()]).is_err());
    }
    #[test]
    fn start_defaults_preserve_explicit_and_environment_filters() {
        let o = parse(&["--start".into(), "--me".into()]).unwrap();
        assert_eq!(
            adaptive_args(&o, false, false),
            ["--me", "--states", "PD", "--sort", "S"]
        );
        assert_eq!(adaptive_args(&o, true, true), ["--me"]);
        let o = parse(&["--start".into(), "--states=R".into(), "-S-i".into()]).unwrap();
        assert_eq!(adaptive_args(&o, false, false), ["--states=R", "-S-i"]);
        let o = parse(&["--start".into(), "--steps".into()]).unwrap();
        assert_eq!(adaptive_args(&o, false, false), ["--steps"]);
    }
    #[test]
    fn refresh_metadata_adapts_to_terminal_width() {
        let stamp = "2030-01-02 03:04:05 +0000";
        for width in 1..=200 {
            let header = refresh_header(Duration::from_secs(5), stamp, width);
            for line in header.lines() {
                assert!(UnicodeWidthStr::width(line) <= width, "{width}: {line:?}");
            }
            if width >= 17 {
                assert!(header.contains("Every 5s"));
                assert!(header.contains("03:04:05"));
            }
        }
        let wide = refresh_header(Duration::from_secs(5), stamp, 80);
        assert!(wide.starts_with("Every 5s"));
        assert!(wide.contains("Updated: 2030-01-02 03:04:05 +0000\n\n"));
        let narrow = refresh_header(Duration::from_secs(5), stamp, 20);
        assert_eq!(narrow, "Every 5s\nUpdated: 03:04:05\n\n");
        assert!(
            refresh_header(Duration::from_secs(30), "unknown", 80).contains("Updated: unknown")
        );
    }
}
