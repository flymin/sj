use super::{App, identity};
use crate::{
    config::{Align, Resolved},
    fields::Row,
    layout, util,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn capacity(height: usize) -> usize {
    height.saturating_sub(6).max(1)
}

#[derive(Default)]
pub struct Geometry {
    pub fields: Vec<String>,
    pub starts: Vec<usize>,
    pub widths: Vec<usize>,
    pub total: usize,
}
fn display(row: &Row, field: &str, column: &crate::config::Column, width: usize) -> String {
    let value = util::clean(
        row.get(&field.to_ascii_lowercase())
            .map_or("", String::as_str)
            .trim(),
    );
    layout::formatted(&value, field, column, width)
}
impl Geometry {
    pub fn new(rows: &[Row], view: &Resolved, available: usize) -> Self {
        let mut result = Self::default();
        let mut natural_widths = Vec::new();
        for (field, column) in &view.columns {
            let natural = rows
                .iter()
                .map(|row| UnicodeWidthStr::width(display(row, field, column, usize::MAX).as_str()))
                .chain(std::iter::once(UnicodeWidthStr::width(column.title(field))))
                .max()
                .unwrap_or(1)
                .max(1)
                .min(column.max_width.unwrap_or(usize::MAX));
            let width = column
                .preferred_width
                .unwrap_or(natural)
                .clamp(column.min_width.min(natural), natural);
            natural_widths.push(natural);
            result.widths.push(width);
            result.fields.push(field.to_ascii_lowercase());
        }
        // preferred_width is an initial allocation, just as in plain output.
        // Keep every column when space is scarce; grow useful content when it fits.
        let initial = result
            .widths
            .iter()
            .fold(result.fields.len().saturating_sub(1), |total, width| {
                total.saturating_add(*width)
            });
        let mut spare = available.saturating_sub(initial);
        let mut grow: Vec<_> = view
            .columns
            .iter()
            .enumerate()
            .map(|(i, (_, column))| (column.grow_priority, i))
            .collect();
        grow.sort_unstable();
        for (_, i) in grow {
            let extra = natural_widths[i]
                .saturating_sub(result.widths[i])
                .min(spare);
            result.widths[i] += extra;
            spare -= extra;
        }
        for (i, width) in result.widths.iter().enumerate() {
            if i > 0 {
                result.total = result.total.saturating_add(1);
            }
            result.starts.push(result.total);
            result.total = result.total.saturating_add(*width);
        }
        result
    }
    pub fn anchor(&self, offset: usize) -> Option<(String, usize)> {
        let index = self.starts.iter().rposition(|start| *start <= offset)?;
        Some((self.fields[index].clone(), offset - self.starts[index]))
    }
    pub fn restore(&self, anchor: Option<(String, usize)>, fallback: usize) -> usize {
        anchor
            .and_then(|(field, delta)| {
                self.fields
                    .iter()
                    .position(|name| *name == field)
                    .map(|i| self.starts[i] + delta.min(self.widths[i].saturating_sub(1)))
            })
            .unwrap_or(fallback)
    }
}

// Crop by terminal cells, preserving a blank when an edge bisects a wide glyph.
fn crop(value: &str, start: usize, width: usize) -> String {
    let end = start.saturating_add(width);
    let mut position = 0;
    let mut used = 0;
    let mut out = String::new();
    for glyph in value.graphemes(true) {
        let size = UnicodeWidthStr::width(glyph);
        if size == 0 {
            if used > 0 && position <= end {
                out.push_str(glyph);
            }
            continue;
        }
        let next = position.saturating_add(size);
        if position >= end {
            break;
        }
        if next > start {
            let take = next.min(end) - position.max(start);
            if position >= start && next <= end {
                out.push_str(glyph);
            } else {
                out.push_str(&" ".repeat(take));
            }
            used += take;
        }
        position = next;
    }
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    out
}
fn highlighted(
    value: &str,
    start: usize,
    width: usize,
    marks: &[(usize, usize)],
    plain: Style,
    hit: Style,
) -> Line<'static> {
    let end = start.saturating_add(width);
    let mut spans = vec![];
    let mut buffer = String::new();
    let mut marked = false;
    let mut position: usize = 0;
    let mut used = 0;
    let mut mark = 0;
    {
        let mut append = |text: &str, selected: bool| {
            if marked != selected && !buffer.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut buffer),
                    if marked { hit } else { plain },
                ));
            }
            marked = selected;
            buffer.push_str(text);
        };
        for (offset, glyph) in value.grapheme_indices(true) {
            while mark < marks.len() && marks[mark].1 <= offset {
                mark += 1;
            }
            let selected = mark < marks.len() && marks[mark].0 < offset + glyph.len();
            let size = UnicodeWidthStr::width(glyph);
            if size == 0 {
                if used > 0 && position <= end {
                    append(glyph, selected);
                }
                continue;
            }
            if position >= end {
                break;
            }
            let next = position.saturating_add(size);
            if next > start {
                let take = next.min(end) - position.max(start);
                if position >= start && next <= end {
                    append(glyph, selected);
                } else {
                    append(&" ".repeat(take), false);
                }
                used += take;
            }
            position = next;
        }
        append(&" ".repeat(width.saturating_sub(used)), false);
    }
    if !buffer.is_empty() {
        spans.push(Span::styled(buffer, if marked { hit } else { plain }));
    }
    Line::from(spans)
}
fn cell_piece(value: &str, total: usize, align: Align, skip: usize, width: usize) -> String {
    let value_width = UnicodeWidthStr::width(value);
    let padding = if matches!(align, Align::Right) {
        total.saturating_sub(value_width)
    } else {
        0
    };
    let leading = padding.saturating_sub(skip).min(width);
    let mut text = " ".repeat(leading);
    text.push_str(&crop(value, skip.saturating_sub(padding), width - leading));
    text
}
fn fit(value: &str, width: usize, overflow: crate::config::Overflow) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.into();
    }
    let ellipsis = matches!(overflow, crate::config::Overflow::Ellipsis) && width > 0;
    let limit = width.saturating_sub(usize::from(ellipsis));
    let mut used = 0;
    let mut text = String::new();
    for glyph in value.graphemes(true) {
        let size = UnicodeWidthStr::width(glyph);
        if used + size > limit {
            break;
        }
        text.push_str(glyph);
        used += size;
    }
    if ellipsis {
        text.push('…');
    }
    text
}
fn line(frame: &mut Frame, area: Rect, text: impl Into<String>, style: Style) {
    if area.width > 0 && area.height > 0 {
        frame.render_widget(
            Paragraph::new(util::clean(&text.into())).style(style),
            Rect { height: 1, ..area },
        );
    }
}
fn full_line(frame: &mut Frame, y: u16, text: impl Into<String>, style: Style) {
    let area = frame.area();
    if y < area.height {
        line(frame, Rect::new(0, y, area.width, 1), text, style);
    }
}
fn table_row(frame: &mut Frame, app: &App, y: u16, row: Option<(usize, &Row)>) {
    let selected = row.is_some_and(|(index, _)| index == app.selection.selected);
    let color = app.setup.color;
    let theme = &app.resolved.theme;
    let base = if selected {
        theme.tui.selected.terminal(color)
    } else if row.is_none() {
        theme.header.terminal(color)
    } else {
        theme.row.terminal(color)
    };
    let width = usize::from(frame.area().width.saturating_sub(3));
    let offset = app.selection.horizontal;
    let end = offset.saturating_add(width);
    let mut spans = Vec::new();
    let mut filled = 0;
    for (i, (field, column)) in app.resolved.columns.iter().enumerate() {
        let start = app.geometry.starts[i];
        let col_end = start.saturating_add(app.geometry.widths[i]);
        let left = start.max(offset);
        let right = col_end.min(end);
        if left >= right {
            continue;
        }
        let x = left - offset;
        if x > filled {
            spans.push(Span::styled(" ".repeat(x - filled), base));
        }
        let value = row.map_or_else(
            || column.title(field).into(),
            |(_, row)| display(row, field, column, app.geometry.widths[i]),
        );
        let value = fit(&value, app.geometry.widths[i], column.overflow);
        let style = match row {
            Some((_, row)) if !selected => layout::style(theme, row, field, column).terminal(color),
            _ => base,
        };
        spans.push(Span::styled(
            cell_piece(
                &value,
                app.geometry.widths[i],
                column.align,
                left - start,
                right - left,
            ),
            style,
        ));
        filled = right - offset;
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(base),
        Rect::new(2, y, width as u16, 1),
    );
    line(
        frame,
        Rect::new(0, y, 2, 1),
        if selected { "> " } else { "  " },
        base,
    );
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    let color = app.setup.color;
    let theme = &app.resolved.theme;
    if width < 24 || height < 8 {
        full_line(
            frame,
            0,
            "Resize terminal · q quit",
            theme.tui.muted.terminal(color),
        );
        return;
    }
    let title = format!(
        " sj · {} · {}",
        if app
            .setup
            .query
            .names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("StepId"))
        {
            "steps"
        } else {
            "queue"
        },
        app.setup.view
    );
    let stamp = app.updated.as_deref().unwrap_or("—");
    let stamp = if width < 100 {
        stamp.split_whitespace().nth(1).unwrap_or(stamp)
    } else {
        stamp
    };
    let right = format!(
        "Every {}s · Updated {} ",
        app.schedule.interval.as_secs(),
        stamp
    );
    let gap = width.saturating_sub(
        UnicodeWidthStr::width(title.as_str()) + UnicodeWidthStr::width(right.as_str()),
    );
    full_line(
        frame,
        0,
        if gap > 0 {
            format!("{title}{}{right}", " ".repeat(gap))
        } else {
            format!("{title} · {}s", app.schedule.interval.as_secs())
        },
        theme.tui.title.terminal(color),
    );
    let running = app
        .rows
        .iter()
        .filter(|row| {
            row.get("state")
                .or_else(|| row.get("stepstate"))
                .is_some_and(|s| s.starts_with("RUNNING"))
        })
        .count();
    let pending = app
        .rows
        .iter()
        .filter(|row| {
            row.get("state")
                .or_else(|| row.get("stepstate"))
                .is_some_and(|s| s.starts_with("PENDING"))
        })
        .count();
    let status = if app.error.is_some() {
        "STALE"
    } else if app.schedule.paused {
        "PAUSED"
    } else if app.queue_task.is_some() {
        "UPDATING"
    } else {
        "LIVE"
    };
    full_line(
        frame,
        1,
        format!(
            " Jobs {}/{} · RUNNING {running} · PENDING {pending}    {status}",
            app.rows.len(),
            app.fetched
        ),
        theme.status.terminal(color),
    );
    if let Some(error) = &app.error {
        full_line(
            frame,
            2,
            format!(" {error} · r retry"),
            theme.tui.error.terminal(color),
        );
    } else {
        full_line(
            frame,
            2,
            "─".repeat(width),
            theme.tui.border.terminal(color),
        );
    }
    table_row(frame, app, 3, None);
    let body = capacity(height);
    for (i, row) in app
        .rows
        .iter()
        .enumerate()
        .skip(app.selection.top)
        .take(body)
    {
        table_row(
            frame,
            app,
            4 + (i - app.selection.top) as u16,
            Some((i, row)),
        );
    }
    if app.rows.is_empty() {
        full_line(
            frame,
            4,
            if app.updated.is_none() && app.error.is_none() {
                " Loading queue…"
            } else {
                " No jobs match the current filters"
            },
            theme.tui.muted.terminal(color),
        );
    }
    if app.rows.len() > body {
        let thumb = (body * body / app.rows.len()).max(1);
        let top = (body - thumb) * app.selection.top / app.rows.len().saturating_sub(body).max(1);
        for i in 0..body {
            line(
                frame,
                Rect::new(area.width - 1, 4 + i as u16, 1, 1),
                if i >= top && i < top + thumb {
                    "┃"
                } else {
                    "│"
                },
                if i >= top && i < top + thumb {
                    theme.tui.key.terminal(color)
                } else {
                    theme.tui.border.terminal(color)
                },
            );
        }
    }
    let chosen = app
        .rows
        .get(app.selection.selected)
        .map(identity)
        .map_or("—".into(), |(_, id)| id);
    let first = app
        .geometry
        .starts
        .iter()
        .zip(&app.geometry.widths)
        .position(|(start, size)| start.saturating_add(*size) > app.selection.horizontal)
        .map_or(0, |i| i + 1);
    let last = app
        .geometry
        .starts
        .iter()
        .rposition(|start| {
            *start
                < app
                    .selection
                    .horizontal
                    .saturating_add(width.saturating_sub(3))
        })
        .map_or(0, |i| i + 1);
    full_line(
        frame,
        area.height - 2,
        format!(
            " Selected {chosen} · Row {}/{} · Columns {first}–{last}/{} {}",
            if app.rows.is_empty() {
                0
            } else {
                app.selection.selected + 1
            },
            app.rows.len(),
            app.geometry.fields.len(),
            if app.geometry.total > width.saturating_sub(3) {
                "↔"
            } else {
                ""
            }
        ),
        theme.tui.muted.terminal(color),
    );
    let keys = if width >= 100 {
        " ↑↓ Move  ←→ Scroll  PgUp/Dn Page  Enter Details  Space Pause  +/- Rate  r Refresh  ? Help  q Quit"
    } else if width >= 60 {
        " ↑↓ Move  ←→ Scroll  Enter Details  Space Pause  ? Help  q Quit"
    } else {
        " ↑↓ Move  Enter Details  ? Help  q Quit"
    };
    full_line(frame, area.height - 1, keys, theme.tui.key.terminal(color));
    if app.popup.is_some() {
        draw_popup(frame, app);
    }
    if app.help {
        draw_help(frame, app);
    }
}

fn popup_area(width: usize, height: usize) -> Rect {
    let w = if width < 44 {
        width.saturating_sub(2)
    } else {
        width * 9 / 10
    }
    .clamp(1, 120)
    .min(width);
    let h = (height * 85 / 100).clamp(1, 36).min(height);
    Rect::new(
        ((width - w) / 2) as u16,
        ((height - h) / 2) as u16,
        w as u16,
        h as u16,
    )
}
pub fn popup_capacity(width: usize, height: usize) -> usize {
    usize::from(popup_area(width, height).height.saturating_sub(6)).max(1)
}
pub fn popup_text_width(width: usize, height: usize) -> usize {
    usize::from(popup_area(width, height).width.saturating_sub(2)).max(1)
}
fn wrapped(lines: &[String], width: usize) -> Vec<String> {
    let mut result = Vec::new();
    let width = width.max(1);
    for text in lines {
        let clean = util::clean(text);
        let mut text = String::new();
        let mut used = 0;
        for glyph in clean.graphemes(true) {
            let size = UnicodeWidthStr::width(glyph);
            if used + size > width && used > 0 {
                result.push(std::mem::take(&mut text));
                used = 0;
            }
            if size <= width {
                text.push_str(glyph);
                used += size;
            }
        }
        result.push(text);
    }
    result
}
fn draw_popup(frame: &mut Frame, app: &mut App) {
    let area = popup_area(
        usize::from(frame.area().width),
        usize::from(frame.area().height),
    );
    let color = app.setup.color;
    let theme = &app.resolved.theme;
    let popup = app.popup.as_mut().unwrap();
    let (_, id) = identity(&popup.row);
    let present = app
        .rows
        .iter()
        .any(|row| identity(row) == identity(&popup.row));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" Task {} ", util::clean(&id)))
        .border_style(theme.tui.title.terminal(color))
        .style(theme.row.terminal(color));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < 4 {
        return;
    }
    line(
        frame,
        inner,
        popup.tab.title(),
        theme.tui.title.terminal(color),
    );
    let stdout = popup.tab == super::details::Tab::Stdout;
    let paged_stdout = stdout && popup.stdout.is_some();
    let capacity = usize::from(inner.height - 4);
    let stdout_rows = popup.stdout.as_ref().filter(|_| stdout).map(|snapshot| {
        snapshot.visible_rows(
            usize::from(inner.width),
            popup.stdout_wrap,
            Some(&popup.search_pattern),
            popup.y,
            capacity,
            popup.x,
        )
    });
    let progress = popup
        .stdout
        .as_ref()
        .zip(stdout_rows.as_ref())
        .map(|(snapshot, rows)| {
            snapshot.progress(
                rows,
                popup.y.min(rows.len().saturating_sub(capacity)),
                capacity,
            )
        });
    let line_position = popup
        .stdout
        .as_ref()
        .zip(stdout_rows.as_ref())
        .and_then(|(snapshot, rows)| {
            snapshot.line_position(rows, popup.y.min(rows.len().saturating_sub(capacity)))
        })
        .map(|(current, total)| format!("Line {current}/{total}"))
        .unwrap_or_else(|| {
            if popup.stdout_lines_error.is_some() {
                "Line ?/?".into()
            } else {
                "Line …/… (counting)".into()
            }
        });
    let status = if stdout {
        if app.stdout_task.is_some() {
            "Loading stdout…".into()
        } else if let Some(snapshot) = &popup.stdout {
            format!(
                "{line_position} · {}% · Stdout: {}",
                progress.unwrap_or(0),
                snapshot.updated
            )
        } else {
            "Stdout · press r to retry".into()
        }
    } else if app.detail_task.is_some() {
        "Loading controller details…".into()
    } else if let Some(controller) = &popup.controller {
        format!("Controller: {}", controller.updated)
    } else {
        format!("Queue snapshot: {}", popup.snapshot_time)
    };
    line(
        frame,
        Rect {
            y: inner.y + 1,
            ..inner
        },
        status,
        theme.tui.muted.terminal(color),
    );
    let error = if stdout {
        popup
            .stdout_error
            .as_deref()
            .or(popup.stdout_lines_error.as_deref())
    } else {
        popup.error.as_deref()
    };
    let notice = if stdout && popup.search_edit.is_some() {
        let draft = popup.search_edit.as_deref().unwrap_or("");
        let available = usize::from(inner.width).saturating_sub(27).max(1);
        let size = UnicodeWidthStr::width(draft);
        let draft = if size > available {
            crop(draft, size - available, available)
        } else {
            draft.to_owned()
        };
        format!("/{}▌  Enter Search · Esc Cancel", draft)
    } else if stdout && app.search_task.is_some() {
        format!("Searching /{}/ · Esc Cancel", popup.search_pattern)
    } else if stdout && popup.search_message.is_some() {
        popup.search_message.clone().unwrap_or_default()
    } else if let Some(error) = error {
        error.to_owned()
    } else if stdout {
        popup
            .stdout
            .as_ref()
            .map(|snapshot| snapshot.path.display().to_string())
            .unwrap_or_default()
    } else if !present {
        "Task is not in the latest queue snapshot".into()
    } else {
        String::new()
    };
    line(
        frame,
        Rect {
            y: inner.y + 2,
            ..inner
        },
        notice,
        if error.is_some() {
            theme.tui.error.terminal(color)
        } else {
            theme.tui.muted.terminal(color)
        },
    );
    let mut marks = vec![];
    let lines = if let Some(rows) = stdout_rows {
        if rows.is_empty() {
            vec!["Stdout is empty".into()]
        } else {
            rows.into_iter()
                .map(|row| {
                    marks.push(row.marks);
                    row.text
                })
                .collect()
        }
    } else {
        popup.lines()
    };
    let overview = popup.tab == super::details::Tab::Overview;
    let lines = if !paged_stdout && popup.wrapped() {
        wrapped(&lines, usize::from(inner.width))
    } else {
        lines
    };
    popup.y = popup.y.min(lines.len().saturating_sub(capacity));
    let max_width = lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .max()
        .unwrap_or(0);
    popup.x = if !popup.wrapped() {
        popup
            .x
            .min(max_width.saturating_sub(usize::from(inner.width)))
    } else {
        0
    };
    for (i, text) in lines.iter().skip(popup.y).take(capacity).enumerate() {
        let style = if overview && !text.is_empty() && text.chars().all(|c| !c.is_lowercase()) {
            theme.tui.title.terminal(color)
        } else {
            theme.row.terminal(color)
        };
        let area = Rect::new(inner.x, inner.y + 3 + i as u16, inner.width, 1);
        if let Some(marks) = marks.get(popup.y + i).filter(|marks| !marks.is_empty()) {
            frame.render_widget(
                highlighted(
                    text,
                    popup.x,
                    usize::from(inner.width),
                    marks,
                    style,
                    theme.tui.selected.terminal(color),
                ),
                area,
            );
        } else {
            line(
                frame,
                area,
                crop(&util::clean(text), popup.x, usize::from(inner.width)),
                style,
            );
        }
    }
    line(
        frame,
        Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
        if stdout {
            format!(
                "/ Search · n/N · g/G · w Wrap:{} · r Refresh · Esc Back {}%",
                if popup.stdout_wrap { "on" } else { "off" },
                progress.unwrap_or(0)
            )
        } else {
            format!(
                "Tab Switch · {}↑↓/Pg Scroll · r Refresh · Esc Back  {}/{}",
                if popup.tab == super::details::Tab::Raw {
                    if popup.raw_wrap {
                        "w Wrap:on · "
                    } else {
                        "w Wrap:off · "
                    }
                } else {
                    ""
                },
                popup.y + 1,
                lines.len().max(1)
            )
        },
        theme.tui.key.terminal(color),
    );
}
fn draw_help(frame: &mut Frame, app: &mut App) {
    let area = popup_area(
        usize::from(frame.area().width),
        usize::from(frame.area().height),
    );
    let theme = &app.resolved.theme;
    let color = app.setup.color;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Queue shortcuts ")
        .border_style(theme.tui.title.terminal(color));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let text = [
        "NAVIGATION",
        "↑ / ↓          Previous / next task",
        "← / →          Scroll horizontally by 8 cells",
        "Ctrl + ← / →   Scroll horizontally by a page",
        "PgUp / PgDn    Previous / next page",
        "Home / End     First / last task",
        "Enter          Open task details",
        "",
        "REFRESH",
        "Space          Pause / resume automatic queries",
        "+ / -          Faster / slower (1-second steps)",
        "r              Refresh queue; in details, refresh task",
        "",
        "DETAILS",
        "Tab / Shift+Tab  Next / previous detail tab",
        "Stdout         Loads on first visit; r reloads the tail",
        "/              Enter Stdout search; Enter confirms, Esc cancels",
        "n / N          Next / previous Stdout match",
        "g / G          Stdout beginning / end",
        "j k h l        Stdout down/up/left/right",
        "f/b · d/u      Full / half page (Ctrl also works)",
        "↑↓ / PgUp/Dn   Scroll details",
        "← / →          Scroll long Raw / Stdout lines",
        "w              Toggle Raw / Stdout wrapping (default off)",
        "Home / End     Beginning / end of details",
        "",
        "EXIT",
        "q / Esc        Close popup, or quit from queue",
        "Ctrl+C         Quit from any screen",
        "",
        "Columns keep their configured width limits.",
        "Job filters always use complete raw values.",
    ];
    let lines = wrapped(
        &text.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        usize::from(inner.width),
    );
    let count = usize::from(inner.height.saturating_sub(1));
    app.help_y = app.help_y.min(lines.len().saturating_sub(count));
    for (i, text) in lines.iter().skip(app.help_y).take(count).enumerate() {
        line(
            frame,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            text,
            theme.row.terminal(color),
        );
    }
    line(
        frame,
        Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
        "↑↓/Pg Scroll · q/Esc Close",
        theme.tui.key.terminal(color),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        queue::Query,
        tui::{App, Setup},
    };
    use std::time::Duration;
    #[test]
    fn search_highlights_preserve_unicode_clipping_and_terminal_width() {
        let value = "ab训练cd";
        let plain = Style::default();
        let hit = Style::default().fg(ratatui::style::Color::Yellow);
        for start in 0..8 {
            for width in 0..9 {
                let marked = highlighted(value, start, width, &[(2, 8)], plain, hit);
                let text = marked
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                assert_eq!(text, crop(value, start, width));
            }
        }
        assert!(
            highlighted(value, 0, 8, &[(2, 8)], plain, hit)
                .spans
                .iter()
                .any(|span| span.style == hit)
        );
    }
    fn app() -> App {
        App::new(Setup {
            config: Config::builtin(),
            view: "queue".into(),
            query: Query {
                native: vec![],
                names: vec!["JobId".into()],
                view_filter: None,
                filters: vec![],
            },
            interval: Duration::from_secs(5),
            color: true,
        })
        .unwrap()
    }
    #[test]
    fn wide_glyphs_and_partial_cells_never_overflow_the_viewport() {
        for start in 0..12 {
            for width in 0..12 {
                assert_eq!(
                    UnicodeWidthStr::width(crop("ab你好世界cd", start, width).as_str()),
                    width
                );
            }
        }
        assert_eq!(crop("a你好b", 2, 3), " 好");
        assert_eq!(cell_piece("42", 20, Align::Right, 16, 4), "  42");
        for start in 0..8 {
            for width in 0..8 {
                assert_eq!(
                    UnicodeWidthStr::width(crop("a👩‍💻e\u{301}你好", start, width).as_str()),
                    width
                );
            }
        }
        assert_eq!(fit("👩‍💻ab", 3, crate::config::Overflow::Ellipsis), "👩‍💻…");
        assert_eq!(wrapped(&["a👩‍💻bc".into()], 3), ["a👩‍💻", "bc"]);
    }
    #[test]
    fn all_sizes_popups_and_monochrome_render_within_the_frame() {
        let mut app = app();
        app.rows = vec![Row::from([
            ("jobid".into(), "42".into()),
            ("name".into(), "训练-task-long-name".into()),
            ("state".into(), "RUNNING".into()),
        ])];
        for color in [false, true] {
            for (width, height) in [(1, 1), (20, 6), (40, 10), (80, 24), (140, 40)] {
                for overlay in 0..3 {
                    app.setup.color = color;
                    app.reflow(width, height).unwrap();
                    app.help = overlay == 2;
                    app.popup = if overlay == 1 {
                        Some(super::super::details::Popup::new(
                            app.rows[0].clone(),
                            "now".into(),
                        ))
                    } else {
                        None
                    };
                    let backend = ratatui::backend::TestBackend::new(width as u16, height as u16);
                    let mut terminal = ratatui::Terminal::new(backend).unwrap();
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                    if !color {
                        for cell in terminal.backend().buffer().content() {
                            assert_eq!(cell.fg, ratatui::style::Color::Reset);
                            assert_eq!(cell.bg, ratatui::style::Color::Reset);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn tui_keeps_all_ordered_columns_and_obeys_explicit_column_widths() {
        let config=Config::parse("[views.test]\norder=['JobId','Name']\n[views.test.columns.Name]\npreferred_width=12\nmax_width=15\noptional=true\n[[views.test.rules]]\nmin_width=100\norder=['JobId','Partition','Name']").unwrap();
        let row = Row::from([("name".into(), "x".repeat(80))]);
        let view = config.resolve_tui("test", 30).unwrap();
        let geometry = Geometry::new(&[row], &view, 20);
        assert_eq!(geometry.fields, ["jobid", "name", "partition"]);
        assert_eq!(geometry.widths[1], 12);
    }
    #[test]
    fn spare_screen_width_expands_content_by_priority_without_hiding_columns() {
        let config=Config::parse("[views.test]\norder=['Name','ReasonList']\n[views.test.columns.Name]\npreferred_width=8\ngrow_priority=0\n[views.test.columns.ReasonList]\npreferred_width=10\ngrow_priority=10").unwrap();
        let row = Row::from([
            ("name".into(), "x".repeat(40)),
            ("reasonlist".into(), "y".repeat(70)),
        ]);
        let view = config.resolve_tui("test", 80).unwrap();
        let geometry = Geometry::new(std::slice::from_ref(&row), &view, 60);
        assert_eq!(geometry.widths, [40, 19]);
        assert_eq!(geometry.total, 60);
        let narrow = Geometry::new(std::slice::from_ref(&row), &view, 15);
        assert_eq!(narrow.widths, [8, 15]);
        assert_eq!(narrow.fields.len(), 2);
        assert!(narrow.total > 15);
        let wide = Geometry::new(std::slice::from_ref(&row), &view, 200);
        assert_eq!(wide.widths, [40, 70]);
        assert_eq!(wide.total, 111);
        let capped=Config::parse("[views.test]\norder=['Name','ReasonList']\n[views.test.columns.Name]\npreferred_width=8\nmax_width=20\ngrow_priority=0\n[views.test.columns.ReasonList]\npreferred_width=10\ngrow_priority=10").unwrap();
        let capped = Geometry::new(&[row], &capped.resolve_tui("test", 80).unwrap(), 60);
        assert_eq!(capped.widths, [20, 39]);
        assert_eq!(capped.total, 60);
    }
}
