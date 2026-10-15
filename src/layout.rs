use crate::config::{Align, Column, Format, Overflow, Resolved};
use crate::fields::Row;
use crate::theme::{StateScope, Style, Theme};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn truncate(s: &str, max: usize) -> String {
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
            [s] => (0, 0, *s),
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
    let (major, suffix, minor, next) = if seconds >= 86400 {
        (seconds / 86400, 'd', seconds % 86400 / 3600, 'h')
    } else if seconds >= 3600 {
        (seconds / 3600, 'h', seconds % 3600 / 60, 'm')
    } else if seconds >= 60 {
        (seconds / 60, 'm', seconds % 60, 's')
    } else {
        return format!("{seconds}s");
    };
    let detail = format!("{major}{suffix}{minor:02}{next}");
    if detail.len() <= width {
        detail
    } else {
        clip(&format!("{major}{suffix}"), width)
    }
}
fn reason(value: &str) -> &str {
    match value {
        "None" | "(null)" | "N/A" => "",
        "Priority" => "Prio",
        "Resources" => "Res",
        "Dependency" => "Dep",
        "DependencyNeverSatisfied" => "DepNever",
        "JobHeldUser" => "HeldUser",
        "JobHeldAdmin" => "HeldAdmin",
        "ReqNodeNotAvail" => "NodeUnavail",
        other => other,
    }
}
fn formatted(value: &str, name: &str, column: &Column, width: usize) -> String {
    let format = match column.format {
        Format::Auto => match name.to_ascii_lowercase().as_str() {
            "timeused" | "timeleft" | "timelimit" | "mintime" | "pendingtime" => Format::Duration,
            "starttime" | "endtime" | "submittime" | "eligibletime" | "accruetime" | "deadline"
            | "lastschedeval" | "preempttime" | "resizetime" => Format::Timestamp,
            "reason" => Format::Reason,
            _ => Format::Raw,
        },
        other => other,
    };
    match format {
        Format::Duration => compact_time(value, width),
        Format::Reason => reason(value).into(),
        Format::Timestamp
            if width < 19
                && value.len() == 19
                && value.is_ascii()
                && value.as_bytes()[10] == b'T' =>
        {
            format!("{} {}", &value[5..10], &value[11..16])
        }
        _ => value.into(),
    }
}
fn style(theme: &Theme, row: &Row, field: &str, column: &Column) -> Style {
    let state = row
        .get("state")
        .or_else(|| row.get("stepstate"))
        .map(String::as_str)
        .unwrap_or("")
        .split('+')
        .next()
        .unwrap_or("");
    let state_style = theme.states.get(state);
    let mut result = theme.row.clone();
    let scope = theme.state_scope.unwrap_or_default();
    if matches!(scope, StateScope::Row)
        && let Some(state) = state_style
    {
        result.overlay(state);
    }
    result.overlay(&column.style);
    if matches!(scope, StateScope::State)
        && matches!(
            field.to_ascii_lowercase().as_str(),
            "state" | "statecompact" | "stepstate"
        )
        && let Some(state) = state_style
    {
        result.overlay(state);
    }
    result
}
fn wrap(value: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut used = 0;
    for c in value.chars() {
        let size = c.width().unwrap_or(0);
        if used + size > width && used > 0 {
            lines.push(String::new());
            used = 0;
        }
        if size > width {
            lines.last_mut().unwrap().push('?');
            used += 1;
        } else {
            lines.last_mut().unwrap().push(c);
            used += size;
        }
    }
    lines
}

pub fn render(data: &[Row], view: &Resolved, width: usize, noheader: bool, color: bool) -> String {
    let width = width.max(1);
    let count = view.columns.len();
    let natural: Vec<_> = view
        .columns
        .iter()
        .map(|(field, column)| {
            let demand = data
                .iter()
                .map(|row| {
                    UnicodeWidthStr::width(
                        formatted(
                            row.get(&field.to_ascii_lowercase())
                                .map_or("", String::as_str),
                            field,
                            column,
                            usize::MAX,
                        )
                        .as_str(),
                    )
                })
                .chain(std::iter::once(UnicodeWidthStr::width(column.title(field))))
                .max()
                .unwrap_or(1)
                .max(1);
            demand.min(column.max_width.unwrap_or(usize::MAX))
        })
        .collect();
    let floors: Vec<_> = view
        .columns
        .iter()
        .enumerate()
        .map(|(i, (_, column))| column.min_width.min(natural[i]))
        .collect();
    let mut widths: Vec<_> = view
        .columns
        .iter()
        .enumerate()
        .map(|(i, (_, column))| {
            column
                .preferred_width
                .unwrap_or(natural[i])
                .clamp(floors[i], natural[i])
        })
        .collect();
    let mut visible = vec![true; count];
    let total = |widths: &[usize], visible: &[bool]| {
        widths
            .iter()
            .zip(visible)
            .filter(|(_, v)| **v)
            .map(|(n, _)| *n)
            .sum::<usize>()
            + visible.iter().filter(|v| **v).count().saturating_sub(1)
    };
    let mut actions = Vec::new();
    for (i, (_, column)) in view.columns.iter().enumerate() {
        actions.push((column.shrink_priority, 0, i));
        if column.optional {
            actions.push((column.hide_priority, 1, i));
        }
    }
    actions.sort_unstable();
    for (_, kind, i) in actions {
        let shortage = total(&widths, &visible).saturating_sub(width);
        if shortage == 0 {
            break;
        }
        if !visible[i] {
            continue;
        }
        if kind == 0 {
            widths[i] -= shortage.min(widths[i] - floors[i]);
        } else {
            visible[i] = false;
        }
    }
    let mut out = String::new();
    if total(&widths, &visible) > width {
        // Mandatory fields survive on narrow terminals; wrap complete values.
        for (index, row) in data.iter().enumerate() {
            if index != 0 {
                out.push('\n');
            }
            for (i, (field, column)) in view.columns.iter().enumerate() {
                if !visible[i] {
                    continue;
                }
                let value = formatted(
                    row.get(&field.to_ascii_lowercase())
                        .map_or("", String::as_str),
                    field,
                    column,
                    usize::MAX,
                );
                let value = if noheader {
                    value
                } else {
                    format!("{}: {value}", column.title(field))
                };
                for line in wrap(&value, width) {
                    out.push_str(&style(&view.theme, row, field, column).paint(&line, color));
                    out.push('\n');
                }
            }
        }
        return out;
    }
    let mut grow: Vec<_> = view
        .columns
        .iter()
        .enumerate()
        .map(|(i, (_, c))| (c.grow_priority, i))
        .collect();
    grow.sort_unstable();
    for (_, i) in grow {
        if visible[i] {
            widths[i] += natural[i]
                .saturating_sub(widths[i])
                .min(width.saturating_sub(total(&widths, &visible)));
        }
    }
    let kept: Vec<_> = (0..count).filter(|i| visible[*i]).collect();
    let mut line = |row: Option<&Row>| {
        for (position, i) in kept.iter().copied().enumerate() {
            if position != 0 {
                let gap = row.map_or_else(
                    || view.theme.header.clone(),
                    |row| style(&view.theme, row, "", &Column::default()),
                );
                out.push_str(&gap.paint(" ", color));
            }
            let (field, column) = &view.columns[i];
            let value = row.map_or_else(
                || column.title(field).into(),
                |row| {
                    formatted(
                        row.get(&field.to_ascii_lowercase())
                            .map_or("", String::as_str),
                        field,
                        column,
                        widths[i],
                    )
                },
            );
            let value = match column.overflow {
                Overflow::Ellipsis => clip(&value, widths[i]),
                Overflow::Truncate => truncate(&value, widths[i]),
            };
            let padding = widths[i].saturating_sub(UnicodeWidthStr::width(value.as_str()));
            let value = match column.align {
                Align::Right => format!("{}{value}", " ".repeat(padding)),
                Align::Left if position + 1 != kept.len() => {
                    format!("{value}{}", " ".repeat(padding))
                }
                Align::Left => value,
            };
            let style = row.map_or_else(
                || view.theme.header.clone(),
                |row| style(&view.theme, row, field, column),
            );
            out.push_str(&style.paint(&value, color));
        }
        out.push('\n');
    };
    if !noheader {
        line(None);
    }
    for row in data {
        line(Some(row));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    fn sample() -> Row {
        [
            ("jobid", "123456789_123"),
            (
                "name",
                "训练实验-long-abcdefghijklmnopqrstuvwxyz-0123456789",
            ),
            ("username", "example-long-user"),
            ("statecompact", "PD"),
            ("state", "PENDING"),
            ("timeused", "12:12:12"),
            ("numnodes", "32"),
            ("reason", "Priority"),
            ("starttime", "2030-01-02T03:04:05"),
            ("partition", "compute"),
            ("reasonlist", "(Priority)"),
            ("timeleft", "1-02:03:04"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    #[test]
    fn defaults_fit_all_widths_and_restore_wide_columns() {
        let c = Config::builtin();
        for width in 1..=250 {
            let out = render(
                &[sample()],
                &c.resolve("queue", width).unwrap(),
                width,
                false,
                false,
            );
            for line in out.lines() {
                assert!(UnicodeWidthStr::width(line) <= width, "{width}: {line:?}");
            }
            if width >= 200 {
                for value in [
                    "PARTITION",
                    "NODELIST(REASON)",
                    "TIME_LEFT",
                    "123456789_123",
                    "(Priority)",
                ] {
                    assert!(out.contains(value), "{out}");
                }
            }
        }
        let narrow = render(
            &[sample()],
            &c.resolve("queue", 125).unwrap(),
            125,
            false,
            false,
        );
        assert!(!narrow.contains("TIME_LEFT"));
        assert!(narrow.contains("PARTITION"));
    }
    #[test]
    fn priorities_alignment_and_natural_floor() {
        let c = Config::parse("[views.test]\norder=['JobId','Name','TimeLeft']\n[views.test.columns.Name]\nmin_width=2\npreferred_width=8\n[views.test.columns.JobId]\nmin_width=2\n[views.test.columns.TimeLeft]\nhide_priority=0").unwrap();
        let row = [
            ("jobid".into(), "42".into()),
            ("name".into(), "abcdefghijk".into()),
            ("timeleft".into(), "1:23:45".into()),
        ]
        .into_iter()
        .collect();
        let out = render(&[row], &c.resolve("test", 15).unwrap(), 15, false, false);
        assert!(!out.contains("TIME_LEFT"));
        assert!(out.contains("   42 abcdefgh…"));
    }
    #[test]
    fn stacked_view_preserves_complete_mandatory_values() {
        let c = Config::parse("[views.test]\norder=['JobId','Name']").unwrap();
        let out = render(&[sample()], &c.resolve("test", 5).unwrap(), 5, true, false);
        assert!(out.replace('\n', "").contains("123456789_123"));
        assert!(out.replace('\n', "").contains("训练实验-long"));
    }
    #[test]
    fn unicode_and_duration() {
        assert_eq!(clip("你好世界", 5), "你好…");
        assert_eq!(clip("e\u{301}abc", 3), "e\u{301}a…");
        assert_eq!(truncate("你好世界", 5), "你好");
        for (value, width, expected) in [
            ("0:04", 5, "0:04"),
            ("1-02:03:04", 5, "1d02h"),
            ("1:23:45", 5, "1h23m"),
            ("12:34:56", 5, "12h"),
            ("123:45", 5, "2h03m"),
            ("00:00:04", 5, "4s"),
            ("UNLIMITED", 5, "∞"),
            ("NOT_SET", 5, "?"),
            ("9999999999999999999999-01:02:03", 5, "?"),
            ("12:34:56", 8, "12:34:56"),
        ] {
            assert_eq!(compact_time(value, width), expected);
        }
    }
    #[test]
    fn colors_do_not_change_width_and_scope_is_respected() {
        let c = Config::builtin();
        let view = c.resolve("queue", 80).unwrap();
        let plain = render(&[sample()], &view, 80, false, false);
        let painted = render(&[sample()], &view, 80, false, true);
        assert!(!plain.contains('\x1b'));
        assert!(painted.contains("\x1b[33mPD"));
        let mut stripped = String::new();
        let mut escape = false;
        for c in painted.chars() {
            if c == '\x1b' {
                escape = true;
            } else if escape {
                if c == 'm' {
                    escape = false;
                }
            } else {
                stripped.push(c);
            }
        }
        assert_eq!(stripped, plain);
    }
    #[test]
    fn action_ties_shrink_before_hide_and_growth_obeys_priority() {
        let row: Row = [
            ("alpha".into(), "aaaaaaaa".into()),
            ("beta".into(), "bbbbbbbb".into()),
        ]
        .into_iter()
        .collect();
        let c = Config::parse("[views.test]\norder=['Alpha','Beta']\n[views.test.columns.Alpha]\nmin_width=2\noptional=true\nshrink_priority=0\nhide_priority=0").unwrap();
        assert_eq!(
            render(
                std::slice::from_ref(&row),
                &c.resolve("test", 13).unwrap(),
                13,
                true,
                false
            ),
            "aaa… bbbbbbbb\n"
        );
        let c = Config::parse("[views.test]\norder=['Alpha','Beta']\n[views.test.columns.Alpha]\npreferred_width=3\ngrow_priority=5\n[views.test.columns.Beta]\npreferred_width=3\ngrow_priority=0").unwrap();
        assert_eq!(
            render(&[row], &c.resolve("test", 12).unwrap(), 12, true, false),
            "aa… bbbbbbbb\n"
        );
    }
    #[test]
    fn unicode_titles_caps_and_short_values_use_display_cells() {
        let c = Config::parse("[views.test]\norder=['JobId','Account']\n[views.test.columns.JobId]\ntitle='作业'\n[views.test.columns.Account]\ntitle='组'\nmax_width=3").unwrap();
        let row = [
            ("jobid".into(), "42".into()),
            ("account".into(), "界界".into()),
        ]
        .into_iter()
        .collect();
        let out = render(&[row], &c.resolve("test", 20).unwrap(), 20, false, false);
        assert!(out.starts_with("作业 组\n"));
        assert!(out.ends_with("  42 界…\n"));
    }
}
