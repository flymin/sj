use crate::util::{self, Result};
use std::io::{self, IsTerminal};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SEP: char = '\u{1f}';
const FORMAT: &str = "%i\x1f%j\x1f%u\x1f%t\x1f%M\x1f%D\x1f%r\x1f%S\x1f%P\x1f%R\x1f%L";
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
    noheader: bool,
    passthrough: bool,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let width = a == "--width" || a.starts_with("--width=");
        let iterate = a == "-i"
            || a == "--iterate"
            || a.starts_with("--iterate=")
            || (a.starts_with("-i") && a.len() > 2 && !a.starts_with("--"));
        if width || iterate {
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
            let n: usize = value
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{a} requires a positive integer"))?;
            if width {
                o.width = Some(n);
            } else {
                o.interval = Some(Duration::from_secs(n as u64));
            }
        } else {
            if a == "--start" {
                o.start = true;
            }
            if a == "--noheader" {
                o.noheader = true;
            }
            if matches!(
                a,
                "-o" | "-O"
                    | "--format"
                    | "--Format"
                    | "--json"
                    | "--yaml"
                    | "-s"
                    | "--step"
                    | "--steps"
                    | "-l"
                    | "--long"
                    | "--only-job-state"
                    | "--helpformat"
                    | "--helpFormat"
                    | "--helpstate"
                    | "--usage"
                    | "-V"
                    | "--version"
            ) || [
                "--format=",
                "--Format=",
                "--json=",
                "--yaml=",
                "--step=",
                "--steps=",
                "-o",
                "-O",
            ]
            .iter()
            .any(|p| a.starts_with(p))
            {
                o.passthrough = true;
            }
            o.native.push(args[i].clone());
        }
        i += 1;
    }
    Ok(o)
}

pub fn terminal_width() -> usize {
    crate::terminal::size().0
}

pub fn clip(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.into();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = truncate(s, max - 1);
    out.push('…');
    out
}

fn truncate(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

fn compact_time(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.into();
    }
    if value == "UNLIMITED" {
        return "∞".into();
    }
    let (days, clock) = match value.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok(), clock),
        None => (Some(0), value),
    };
    let parts: Option<Vec<u64>> = clock.split(':').map(|s| s.parse().ok()).collect();
    let seconds = days.zip(parts).and_then(|(days, parts)| {
        let (hours, minutes, seconds) = match parts.as_slice() {
            [h, m, s] if *m < 60 && *s < 60 => (*h, *m, *s),
            [m, s] if *s < 60 => (0, *m, *s),
            _ => return None,
        };
        days.checked_mul(86400)?
            .checked_add(hours.checked_mul(3600)?)?
            .checked_add(minutes.checked_mul(60)?)?
            .checked_add(seconds)
    });
    let Some(seconds) = seconds else {
        return "?".into();
    };
    let (major, suffix, minor, next_suffix) = if seconds >= 86400 {
        (seconds / 86400, 'd', seconds % 86400 / 3600, 'h')
    } else if seconds >= 3600 {
        (seconds / 3600, 'h', seconds % 3600 / 60, 'm')
    } else if seconds >= 60 {
        (seconds / 60, 'm', seconds % 60, 's')
    } else {
        return format!("{seconds}s");
    };
    let detailed = format!("{major}{suffix}{minor:02}{next_suffix}");
    if detailed.len() <= width {
        detailed
    } else {
        clip(&format!("{major}{suffix}"), width)
    }
}

fn reason(s: &str) -> String {
    match s {
        "None" | "(null)" | "N/A" => String::new(),
        "Priority" => "Prio".into(),
        "Resources" => "Res".into(),
        "Dependency" => "Dep".into(),
        "DependencyNeverSatisfied" => "DepNever".into(),
        "JobHeldUser" => "HeldUser".into(),
        "JobHeldAdmin" => "HeldAdmin".into(),
        "ReqNodeNotAvail" => "NodeUnavail".into(),
        other => util::clean(other),
    }
}

fn rows(text: &str, start: bool, width: usize) -> Result<Vec<Vec<String>>> {
    text.lines().filter(|s| !s.is_empty()).map(|line| {
        let fields: Vec<_> = line.split(SEP).collect();
        if fields.len() != 11 {
            return Err("unexpected squeue record (possibly a cluster heading or control character in a job name); use --json or an explicit --format for native output".into());
        }
        let mut time = util::clean(if start { fields[7] } else { fields[4] });
        if start && width < 100 && time.len() == 19 && time.is_ascii()
            && time.as_bytes()[10] == b'T' { time = format!("{} {}", &time[5..10], &time[11..16]); }
        Ok(vec![util::clean(fields[0]), util::clean(fields[1]), util::clean(fields[2]),
            util::clean(fields[3]), time, util::clean(fields[5]), reason(fields[6]),
            util::clean(fields[8]), util::clean(fields[9]), util::clean(fields[10])])
    }).collect()
}

pub fn render(text: &str, width: usize, start: bool, noheader: bool) -> Result<String> {
    let data = rows(text, start, width)?;
    let headers = [
        "JOBID",
        "NAME",
        "USER",
        "ST",
        if start { "START_TIME" } else { "TIME" },
        "NODES",
        "REASON",
        "PARTITION",
        "NODELIST(REASON)",
        "TIME_LEFT",
    ];
    let caps = [
        usize::MAX,
        usize::MAX,
        usize::MAX,
        4,
        19,
        8,
        12,
        20,
        usize::MAX,
        19,
    ];
    let mut widths: Vec<usize> = (0..headers.len())
        .map(|c| {
            data.iter()
                .map(|r| UnicodeWidthStr::width(r[c].as_str()))
                .chain(std::iter::once(UnicodeWidthStr::width(headers[c])))
                .max()
                .unwrap_or(1)
                .min(caps[c])
        })
        .collect();
    let mut cols: Vec<usize> = (0..7).collect();
    let budget = |cols: &[usize], widths: &[usize]| {
        width.saturating_sub(
            cols.iter()
                .filter(|c| **c != 1)
                .map(|c| widths[*c])
                .sum::<usize>()
                + cols.len().saturating_sub(1),
        )
    };
    // Keep useful job-name space before adding optional wide-screen columns.
    let preferred_name = widths[1].min(48);
    let compact_fixed = cols
        .iter()
        .filter(|c| **c != 1)
        .map(|c| widths[*c])
        .sum::<usize>()
        + cols.len()
        - 1;
    if width > compact_fixed + preferred_name + widths[7] {
        cols.insert(1, 7);
        let mut wide_cols = cols.clone();
        wide_cols.retain(|c| *c != 6);
        wide_cols.push(8);
        let min_nodes_width = headers[8].len();
        let wide_fixed = wide_cols
            .iter()
            .filter(|c| **c != 1 && **c != 8)
            .map(|c| widths[*c])
            .sum::<usize>()
            + wide_cols.len()
            - 1;
        if width >= wide_fixed + preferred_name + min_nodes_width {
            cols = wide_cols;
            widths[8] = min_nodes_width;
        }
    }
    // Add remaining time last, so it disappears before other columns shrink.
    if cols.contains(&8) && budget(&cols, &widths) > preferred_name + widths[9] {
        let position = cols.iter().position(|c| *c == 4).unwrap();
        cols.insert(position, 9);
    }
    let minimum_name = widths[1].min(8);
    for (col, minimum) in [(4, if start { 11 } else { 5 }), (2, 5), (0, 7)] {
        let fixed: usize = cols.iter().filter(|c| **c != 1).map(|c| widths[*c]).sum();
        let shortage = (fixed + cols.len() - 1 + minimum_name).saturating_sub(width);
        let available = widths[col].saturating_sub(minimum);
        widths[col] -= shortage.min(available);
    }
    if budget(&cols, &widths) < widths[1].min(16) {
        widths[6] = widths[6].min(4);
    }
    for optional in [6, 2, 5, 4, 3] {
        if budget(&cols, &widths) >= widths[1].min(8) {
            break;
        }
        cols.retain(|c| *c != optional);
    }
    let mut out = String::new();
    if width < widths[0] + 3 {
        if !noheader {
            out.push_str(&clip("JOBID / NAME", width.max(1)));
            out.push('\n');
        }
        for row in data {
            out.push_str(&row[0]);
            out.push('\n');
            out.push_str(&clip(&row[1], width.max(1)));
            out.push('\n');
        }
        return Ok(out);
    }
    widths[1] = widths[1].min(budget(&cols, &widths)).max(1);
    if cols.contains(&8) {
        let rest = cols
            .iter()
            .filter(|c| **c != 8)
            .map(|c| widths[*c])
            .sum::<usize>()
            + cols.len()
            - 1;
        let demand = data
            .iter()
            .map(|r| UnicodeWidthStr::width(r[8].as_str()))
            .max()
            .unwrap_or(0)
            .max(headers[8].len());
        widths[8] = demand.min(width.saturating_sub(rest));
    }
    let line = |values: &[String], heading: bool, out: &mut String| {
        for (position, col) in cols.iter().enumerate() {
            if position != 0 {
                out.push(' ');
            }
            let value = if !heading && *col == 2 {
                truncate(&values[*col], widths[*col])
            } else if !heading && (*col == 9 || (*col == 4 && !start)) {
                compact_time(&values[*col], widths[*col])
            } else {
                clip(&values[*col], widths[*col])
            };
            let padding = widths[*col].saturating_sub(UnicodeWidthStr::width(value.as_str()));
            let right = *col == 0 || *col == 5;
            if right {
                out.extend(std::iter::repeat_n(' ', padding));
            }
            out.push_str(&value);
            if !right && position + 1 != cols.len() {
                out.extend(std::iter::repeat_n(' ', padding));
            }
        }
        out.push('\n');
    };
    if !noheader {
        line(
            &headers.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            true,
            &mut out,
        );
    }
    for row in data {
        line(&row, false, &mut out);
    }
    Ok(out)
}

pub fn run(args: Vec<String>) -> Result<i32> {
    if crate::help::show_if_requested(&args, crate::help::QUEUE)? {
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
    let tty = io::stdout().is_terminal();
    if options.interval.is_some() {
        unsafe {
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
    }
    loop {
        let mut command = Command::new("squeue");
        command
            .args(&options.native)
            .args(["--noheader", "--format", FORMAT]);
        command.env("SLURM_BITSTR_LEN", "1048576");
        let text = util::capture(&mut command);
        let signal = SIGNAL.load(Ordering::Relaxed);
        if signal != 0 {
            return Ok(128 + signal);
        }
        let text = text?;
        if options.interval.is_some() && tty {
            util::print("\x1b[H\x1b[2J")?;
        }
        util::print(&render(
            &text,
            options.width.unwrap_or_else(terminal_width),
            options.start,
            options.noheader,
        )?)?;
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
    fn sample() -> String {
        [
            "123456789_123",
            "训练实验-long-abcdefghijklmnopqrstuvwxyz-0123456789",
            "${USER}",
            "PD",
            "12:12:12",
            "32",
            "Priority",
            "2030-01-02T03:04:05",
            "compute",
            "(Priority)",
            "1-02:03:04",
        ]
        .join("\x1f")
            + "\n"
    }
    #[test]
    fn widths_and_id_restoration() {
        for width in [20, 40, 80, 120, 200] {
            let out = render(&sample(), width, false, false).unwrap();
            if width >= 80 {
                assert!(out.contains("123456789_123"));
            } else {
                assert!(out.contains("123456…"));
            }
            for line in out.lines() {
                assert!(UnicodeWidthStr::width(line) <= width, "{width}: {line:?}");
            }
        }
    }
    #[test]
    fn very_narrow_does_not_destroy_id() {
        assert!(
            render(&sample(), 5, false, true)
                .unwrap()
                .contains("123456789_123\n")
        );
    }
    #[test]
    fn wide_columns_restore_partition_and_native_node_reason() {
        let pending = render(&sample(), 200, false, false).unwrap();
        assert!(pending.contains("PARTITION"));
        assert!(pending.contains("NODELIST(REASON)"));
        assert!(pending.contains("(Priority)"));
        let running = sample().replace("(Priority)", "node-[01-16]");
        let wide = render(&running, 200, false, false).unwrap();
        assert!(wide.contains("node-[01-16]"));
        assert!(wide.contains("训练实验-long-abcdefghijklmnopqrstuvwxyz-0123456789"));
        let narrow = render(&running, 80, false, false).unwrap();
        assert!(!narrow.contains("NODELIST(REASON)"));
        for width in 20..=250 {
            for line in render(&running, width, false, false).unwrap().lines() {
                assert!(UnicodeWidthStr::width(line) <= width, "{width}: {line:?}");
            }
        }
    }
    #[test]
    fn unicode_truncation() {
        assert_eq!(clip("你好世界", 5), "你好…");
        assert_eq!(clip("e\u{301}abc", 3), "e\u{301}a…");
        assert_eq!(truncate("你好世界", 5), "你好");
        assert_eq!(truncate("e\u{301}abc", 3), "e\u{301}ab");
    }
    #[test]
    fn time_formats_keep_the_most_significant_units() {
        for (value, width, expected) in [
            ("0:04", 5, "0:04"),
            ("1-02:03:04", 5, "1d02h"),
            ("1:23:45", 5, "1h23m"),
            ("12:34:56", 5, "12h"),
            ("123:45", 5, "2h03m"),
            ("00:00:04", 5, "4s"),
            ("UNLIMITED", 5, "∞"),
            ("NOT_SET", 5, "?"),
            ("INVALID", 5, "?"),
            ("9999999999999999999999-01:02:03", 5, "?"),
            ("12:34:56", 8, "12:34:56"),
        ] {
            assert_eq!(compact_time(value, width), expected);
        }
    }

    #[test]
    fn remaining_time_disappears_before_other_wide_columns() {
        let mut first_remaining = None;
        for width in 20..=250 {
            let out = render(&sample(), width, false, false).unwrap();
            let heading = out.lines().next().unwrap();
            if heading.contains("TIME_LEFT") {
                first_remaining.get_or_insert(width);
                assert!(heading.contains("PARTITION"));
                assert!(heading.contains("NODELIST(REASON)"));
                assert!(heading.split_whitespace().any(|s| s == "TIME"));
                assert!(out.contains("1-02:03:04"));
            } else {
                assert!(
                    first_remaining.is_none(),
                    "remaining time disappeared at {width}"
                );
            }
        }
        let threshold = first_remaining.unwrap();
        let without_remaining = render(&sample(), threshold - 1, false, false).unwrap();
        assert!(without_remaining.contains("PARTITION"));
        assert!(without_remaining.contains("NODELIST(REASON)"));
        let start = render(&sample(), 250, true, false).unwrap();
        assert!(start.contains("START_TIME") && start.contains("TIME_LEFT"));
    }

    #[test]
    fn compression_limits_and_short_fields_do_not_force_padding() {
        let long = sample().replace("${USER}", "example-long-user");
        // All three columns reach their limits before auxiliary columns disappear.
        let out = render(&long, 43, false, false).unwrap();
        assert!(out.contains("123456…"), "{out}");
        assert!(out.contains("examp"), "{out}");
        assert!(!out.contains("examp…"));
        assert!(out.contains("12h"), "{out}");
        assert!(out.lines().next().unwrap().contains("USER"));
        let short = sample()
            .replace("123456789_123", "42")
            .replace("${USER}", "a")
            .replace("12:12:12", "0:00");
        let row = render(&short, 250, false, true).unwrap();
        assert!(row.starts_with("   42 "), "{row}");
        assert!(row.contains(" a    PD "), "{row}");
        assert!(row.contains("0:00 "), "{row}");
    }
    #[test]
    fn start_and_reason() {
        let out = render(&sample(), 120, true, false).unwrap();
        assert!(out.contains("START_TIME"));
        assert!(out.contains("2030-01-02T03:04:05"));
        assert!(out.contains("Prio"));
    }
    #[test]
    fn parses_iteration_without_eating_filters() {
        let args = ["--me", "--start", "-i5", "-t", "PD", "--width=80"].map(str::to_owned);
        let o = parse(&args).unwrap();
        assert_eq!(o.native, ["--me", "--start", "-t", "PD"]);
        assert_eq!(o.interval, Some(Duration::from_secs(5)));
        assert_eq!(o.width, Some(80));
    }
}
