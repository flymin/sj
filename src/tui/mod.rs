mod details;
mod model;
mod process;
mod stdout;
mod view;

use crate::{
    config::{Config, Resolved},
    fields::{Catalog, Row},
    queue::{Query, update_timestamp},
    util::Result,
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use details::{Controller, Popup, Tab};
use model::{Schedule, Selection, identity};
use process::{INTERRUPTED as SIGNAL, Task};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

extern "C" fn interrupted(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::Relaxed);
}

pub(crate) fn catalog(steps: bool) -> Result<Catalog> {
    Catalog::fetch_with(steps, |command| {
        process::capture(command, &AtomicBool::new(false), process::TIMEOUT)
    })
}

pub(crate) struct Setup {
    pub config: Config,
    pub view: String,
    pub query: Query,
    pub interval: Duration,
    pub color: bool,
}

struct Signals(Vec<(libc::c_int, libc::sigaction)>);
impl Signals {
    fn open() -> Result<Self> {
        let mut signals = Self(Vec::new());
        SIGNAL.store(0, Ordering::Relaxed);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut old = unsafe { std::mem::zeroed() };
            action.sa_sigaction = interrupted as *const () as libc::sighandler_t;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            signals.0.push((signal, old));
        }
        Ok(signals)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, old) in &self.0 {
            unsafe {
                libc::sigaction(*signal, old, std::ptr::null_mut());
            }
        }
    }
}
pub(crate) fn with_signals(work: impl FnOnce() -> Result<i32>) -> Result<i32> {
    let _signals = Signals::open()?;
    let result = work();
    let signal = SIGNAL.load(Ordering::Relaxed);
    if signal != 0 {
        Ok(128 + signal)
    } else {
        result
    }
}
struct Screen {
    terminal: Option<ratatui::DefaultTerminal>,
    original: libc::termios,
}
impl Screen {
    fn open() -> Result<Self> {
        let mut original = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
            return Err(format!(
                "read terminal settings: {}",
                io::Error::last_os_error()
            ));
        }
        let mut screen = Self {
            terminal: None,
            original,
        };
        screen.terminal =
            Some(ratatui::try_init().map_err(|e| format!("open fullscreen terminal: {e}"))?);
        Ok(screen)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        if self.terminal.is_some() {
            let _ = ratatui::try_restore();
        }
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
    }
}

struct App {
    setup: Setup,
    rows: Vec<Row>,
    fetched: usize,
    selection: Selection,
    schedule: Schedule,
    updated: Option<String>,
    error: Option<String>,
    queue_task: Option<Task<(Vec<Row>, usize)>>,
    detail_task: Option<Task<(u64, Controller)>>,
    stdout_task: Option<Task<(u64, stdout::Snapshot, stdout::Focus)>>,
    stdout_lines_task: Option<Task<(u64, stdout::Snapshot)>>,
    search_task: Option<Task<(u64, stdout::SearchResult)>>,
    generation: u64,
    popup: Option<Popup>,
    help: bool,
    help_y: usize,
    resolved: Resolved,
    geometry: view::Geometry,
    width: usize,
    height: usize,
    needs_layout: bool,
}
impl App {
    fn new(setup: Setup) -> Result<Self> {
        let resolved = setup.config.resolve_tui(&setup.view, 80)?;
        let schedule = Schedule::new(setup.interval);
        Ok(Self {
            setup,
            rows: Vec::new(),
            fetched: 0,
            selection: Selection::default(),
            schedule,
            updated: None,
            error: None,
            queue_task: None,
            detail_task: None,
            stdout_task: None,
            stdout_lines_task: None,
            search_task: None,
            generation: 0,
            popup: None,
            help: false,
            help_y: 0,
            resolved,
            geometry: view::Geometry::default(),
            width: 0,
            height: 0,
            needs_layout: true,
        })
    }
    fn reflow(&mut self, width: usize, height: usize) -> Result<()> {
        if self.needs_layout || self.width != width || self.height != height {
            let anchor = self.geometry.anchor(self.selection.horizontal);
            self.resolved = self.setup.config.resolve_tui(&self.setup.view, width)?;
            self.geometry =
                view::Geometry::new(&self.rows, &self.resolved, width.saturating_sub(3));
            self.selection.horizontal = self.geometry.restore(anchor, self.selection.horizontal);
            self.width = width;
            self.height = height;
            self.needs_layout = false;
        }
        self.selection
            .visible(self.rows.len(), view::capacity(height));
        self.selection.horizontal = self
            .selection
            .horizontal
            .min(self.geometry.total.saturating_sub(width.saturating_sub(3)));
        Ok(())
    }
    fn work(&mut self) -> Result<bool> {
        let mut changed = false;
        if let Some(result) = self.queue_task.as_ref().and_then(Task::poll) {
            if result
                .as_ref()
                .err()
                .is_some_and(|error| error == process::WORKER_STOPPED)
            {
                return Err(process::WORKER_STOPPED.into());
            }
            self.queue_task = None;
            self.schedule.completed(Instant::now());
            changed = true;
            match result {
                Ok((rows, fetched)) => {
                    self.selection.replace(&self.rows, &rows);
                    self.rows = rows;
                    self.fetched = fetched;
                    self.updated = Some(update_timestamp());
                    self.error = None;
                    self.needs_layout = true;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(result) = self.detail_task.as_ref().and_then(Task::poll) {
            if result
                .as_ref()
                .err()
                .is_some_and(|error| error == process::WORKER_STOPPED)
            {
                return Err(process::WORKER_STOPPED.into());
            }
            self.detail_task = None;
            changed = true;
            if let Some(popup) = &mut self.popup {
                match result {
                    Ok((generation, controller)) if generation == self.generation => {
                        popup.controller = Some(controller);
                        popup.error = None;
                    }
                    Ok(_) => (),
                    Err(error) => popup.error = Some(error),
                }
            }
        }
        if let Some(result) = self.stdout_task.as_ref().and_then(Task::poll) {
            if result
                .as_ref()
                .err()
                .is_some_and(|error| error == process::WORKER_STOPPED)
            {
                return Err(process::WORKER_STOPPED.into());
            }
            self.stdout_task = None;
            changed = true;
            if let Some(popup) = &mut self.popup {
                popup.stdout_loaded = true;
                match result {
                    Ok((generation, mut snapshot, focus)) if generation == self.generation => {
                        if popup.stdout_wrap {
                            snapshot = snapshot.compact(focus);
                        }
                        popup.y = snapshot.focus(
                            focus,
                            view::popup_text_width(self.width, self.height),
                            view::popup_capacity(self.width, self.height),
                            popup.stdout_wrap,
                        );
                        if !matches!(focus, stdout::Focus::Move(_, _)) || popup.stdout_wrap {
                            popup.x = snapshot.horizontal(
                                focus,
                                view::popup_text_width(self.width, self.height),
                                popup.stdout_wrap,
                            );
                        }
                        popup.stdout = Some(snapshot);
                        popup.stdout_error = None;
                    }
                    Ok(_) => (),
                    Err(error) => popup.stdout_error = Some(error),
                }
            }
        }
        if let Some(result) = self.search_task.as_ref().and_then(Task::poll) {
            self.search_task = None;
            changed = true;
            if let Some(popup) = &mut self.popup {
                match result {
                    Ok((
                        generation,
                        stdout::SearchResult::Found {
                            mut snapshot,
                            anchor,
                            wrapped,
                        },
                    )) if generation == self.generation => {
                        let width = view::popup_text_width(self.width, self.height);
                        let focus = stdout::Focus::Found(anchor);
                        if popup.stdout_wrap {
                            snapshot = snapshot.compact(focus);
                        }
                        popup.y = snapshot.focus(
                            focus,
                            width,
                            view::popup_capacity(self.width, self.height),
                            popup.stdout_wrap,
                        );
                        popup.x = snapshot.horizontal(focus, width, popup.stdout_wrap);
                        popup.stdout = Some(snapshot);
                        popup.search_anchor = Some(anchor);
                        popup.search_message = Some(
                            if wrapped {
                                "Search wrapped: match found"
                            } else {
                                "Match found"
                            }
                            .into(),
                        );
                    }
                    Ok((generation, stdout::SearchResult::NotFound))
                        if generation == self.generation =>
                    {
                        popup.search_message = Some("Text not found".into());
                    }
                    Ok(_) => (),
                    Err(error) => popup.search_message = Some(error),
                }
            }
        }
        if let Some(result) = self.stdout_lines_task.as_ref().and_then(Task::poll) {
            self.stdout_lines_task = None;
            changed = true;
            if let Some(popup) = &mut self.popup {
                match result {
                    Ok((generation, counted)) if generation == self.generation => {
                        if let Some(snapshot) = &mut popup.stdout {
                            snapshot.merge_lines(&counted);
                        }
                    }
                    Ok(_) => (),
                    Err(error) => popup.stdout_lines_error = Some(error),
                }
            }
        }
        if self.stdout_task.is_none()
            && let Some(popup) = &self.popup
            && popup.tab == Tab::Stdout
            && popup.stdout_error.is_none()
            && !popup.stdout_wrap
            && let Some(snapshot) = &popup.stdout
            && snapshot.needs_rows(view::popup_capacity(self.width, self.height))
        {
            let snapshot = snapshot.clone();
            let capacity = view::popup_capacity(self.width, self.height);
            let anchor = snapshot
                .rows(
                    view::popup_text_width(self.width, self.height),
                    popup.stdout_wrap,
                )
                .get(popup.y)
                .map_or((0, 0), |row| row.key);
            let generation = self.generation;
            self.stdout_task = Some(Task::spawn_io(move |cancel| {
                stdout::resize(&snapshot, capacity, cancel)
                    .map(|snapshot| (generation, snapshot, stdout::Focus::Move(anchor, 0)))
            }));
            changed = true;
        }
        if self.stdout_lines_task.is_none()
            && self.stdout_task.is_none()
            && let Some(popup) = &self.popup
            && popup.tab == Tab::Stdout
            && popup.stdout_lines_error.is_none()
            && let Some(snapshot) = popup
                .stdout
                .as_ref()
                .filter(|snapshot| snapshot.needs_lines())
        {
            let snapshot = snapshot.clone();
            let generation = self.generation;
            self.stdout_lines_task = Some(Task::spawn_io(move |cancel| {
                stdout::count_lines(&snapshot, cancel).map(|snapshot| (generation, snapshot))
            }));
        }
        if self.schedule.due(Instant::now(), self.queue_task.is_some()) {
            let query = self.setup.query.clone();
            self.schedule.started();
            self.queue_task = Some(Task::spawn(move |cancel| {
                let text = process::capture(&mut query.command(), cancel, process::TIMEOUT)?;
                query.decode(&text)
            }));
            changed = true;
        }
        Ok(changed)
    }
    fn refresh_details(&mut self) {
        self.detail_task = None;
        if let Some(popup) = &mut self.popup {
            self.generation = self.generation.wrapping_add(1);
            let generation = self.generation;
            let row = popup.row.clone();
            popup.error = None;
            self.detail_task = Some(Task::spawn(move |cancel| {
                details::fetch(&row, cancel).map(|value| (generation, value))
            }));
        }
    }
    fn refresh_stdout(&mut self) {
        self.search_task = None;
        self.stdout_task = None;
        self.stdout_lines_task = None;
        if let Some(popup) = &mut self.popup {
            popup.search_anchor = None;
            popup.search_message = None;
            let generation = self.generation;
            let row = popup.row.clone();
            popup.stdout_error = None;
            popup.stdout_lines_error = None;
            let capacity = if popup.stdout_wrap {
                0
            } else {
                view::popup_capacity(self.width, self.height)
            };
            self.stdout_task = Some(Task::spawn_io(move |cancel| {
                stdout::fetch(&row, capacity, cancel)
                    .map(|snapshot| (generation, snapshot, stdout::Focus::End))
            }));
        }
    }
    fn page_stdout(&mut self, page: stdout::Page, focus: stdout::Focus) {
        self.stdout_task = None;
        if let Some(popup) = &mut self.popup
            && let Some(snapshot) = popup.stdout.clone()
        {
            let generation = self.generation;
            popup.stdout_error = None;
            self.stdout_task = Some(Task::spawn_io(move |cancel| {
                stdout::move_page(&snapshot, page, focus, cancel)
                    .map(|snapshot| (generation, snapshot, focus))
            }));
        }
    }
    fn start_search(&mut self, backwards: bool, repeat: bool) {
        self.search_task = None;
        let Some(popup) = &mut self.popup else {
            return;
        };
        if popup.search_pattern.is_empty() {
            popup.search_message = Some("Use / to enter search text".into());
            return;
        }
        let Some(snapshot) = popup.stdout.clone() else {
            popup.search_message = Some("Stdout is not loaded yet".into());
            return;
        };
        let rows = snapshot.rows(
            view::popup_text_width(self.width, self.height),
            popup.stdout_wrap,
        );
        let origin = if repeat { popup.search_anchor } else { None }
            .or_else(|| rows.get(popup.y).map(|row| row.at_column(popup.x)))
            .unwrap_or((0, 0));
        let pattern = popup.search_pattern.clone();
        let generation = self.generation;
        let search_wrap = self.setup.config.tui.search_wrap;
        popup.search_message = None;
        self.stdout_task = None;
        self.search_task = Some(Task::spawn_io(move |cancel| {
            stdout::search(
                &snapshot,
                &pattern,
                origin,
                backwards,
                repeat,
                search_wrap,
                cancel,
            )
            .map(|result| (generation, result))
        }));
    }
    fn edit_search(&mut self, key: KeyEvent) -> bool {
        let Some(popup) = &mut self.popup else {
            return false;
        };
        let Some(draft) = &mut popup.search_edit else {
            return false;
        };
        match key.code {
            KeyCode::Esc => popup.search_edit = None,
            KeyCode::Enter => {
                let draft = popup.search_edit.take().unwrap();
                let repeat = draft.is_empty();
                if !repeat {
                    popup.search_pattern = draft;
                    popup.search_anchor = None;
                }
                self.start_search(false, repeat);
            }
            KeyCode::Backspace => {
                draft.pop();
            }
            KeyCode::Up => *draft = popup.search_pattern.clone(),
            KeyCode::Down => draft.clear(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => draft.clear(),
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && draft.len() + ch.len_utf8() <= 4096 =>
            {
                draft.push(ch);
            }
            _ => (),
        }
        true
    }
    fn toggle_wrap(&mut self) {
        let Some(popup) = &mut self.popup else {
            return;
        };
        if popup.tab == Tab::Stdout {
            let width = view::popup_text_width(self.width, self.height);
            let anchor = popup.stdout.as_ref().and_then(|snapshot| {
                snapshot
                    .rows(width, popup.stdout_wrap)
                    .get(popup.y)
                    .map(|row| row.at_column(popup.x))
            });
            popup.stdout_wrap = !popup.stdout_wrap;
            popup.x = 0;
            if popup.stdout_wrap
                && let Some(anchor) = anchor
                && let Some(snapshot) = &popup.stdout
            {
                popup.stdout = Some(snapshot.compact(stdout::Focus::Found(anchor)));
            }
            if let Some((snapshot, anchor)) = popup.stdout.as_ref().zip(anchor) {
                let rows = snapshot.rows(width, popup.stdout_wrap);
                if let Some(index) = rows.iter().position(|row| row.contains(anchor)) {
                    popup.y = index.min(
                        rows.len()
                            .saturating_sub(view::popup_capacity(self.width, self.height)),
                    );
                    if !popup.stdout_wrap {
                        popup.x = rows[index].column(anchor).unwrap_or(0);
                    }
                }
            }
        } else if popup.tab == Tab::Raw {
            popup.raw_wrap = !popup.raw_wrap;
            popup.x = 0;
            popup.y = 0;
        }
    }
    fn scroll_stdout(&mut self, key: KeyEvent, capacity: usize) {
        self.search_task = None;
        if let Some(popup) = &mut self.popup {
            popup.search_anchor = None;
            popup.search_message = None;
        }
        if self.stdout_task.is_some() {
            if matches!(key.code, KeyCode::Home | KeyCode::End) {
                self.stdout_task = None;
            } else {
                return;
            }
        }
        let Some(popup) = &mut self.popup else {
            return;
        };
        let Some(snapshot) = &popup.stdout else {
            return;
        };
        let width = view::popup_text_width(self.width, self.height);
        let rows = snapshot.rows(width, popup.stdout_wrap);
        if matches!(key.code, KeyCode::Left | KeyCode::Right) {
            if popup.stdout_wrap {
                return;
            }
            let step = if key.modifiers.contains(KeyModifiers::CONTROL) {
                width
            } else {
                8
            };
            let delta = if key.code == KeyCode::Left {
                -(step as isize)
            } else {
                step as isize
            };
            let request =
                if delta < 0 && popup.x < step && popup.y == 0 && snapshot.continues_before() {
                    rows.first().map(|row| {
                        (
                            stdout::Page::Earlier,
                            stdout::Focus::Horizontal(row.at_column(popup.x), delta),
                        )
                    })
                } else if delta > 0
                    && snapshot.continues_after()
                    && rows.len() <= popup.y + capacity
                    && rows.last().is_some_and(|row| {
                        popup.x.saturating_add(step).saturating_add(width)
                            > unicode_width::UnicodeWidthStr::width(row.text.as_str())
                    })
                {
                    rows.last().map(|row| {
                        (
                            stdout::Page::Later,
                            stdout::Focus::Horizontal(row.at_column(popup.x), delta),
                        )
                    })
                } else {
                    None
                };
            if let Some((page, focus)) = request {
                self.page_stdout(page, focus);
            } else {
                popup.x = popup.x.saturating_add_signed(delta);
            }
            return;
        }
        let last = rows.len().saturating_sub(capacity);
        let delta = match key.code {
            KeyCode::Up => -1,
            KeyCode::Down => 1,
            KeyCode::PageUp => -(capacity as isize),
            KeyCode::PageDown => capacity as isize,
            KeyCode::Char('d') => capacity.div_ceil(2) as isize,
            KeyCode::Char('u') => -(capacity.div_ceil(2) as isize),
            _ => 0,
        };
        let request = match key.code {
            KeyCode::Home if snapshot.earlier() => {
                Some((stdout::Page::Start, stdout::Focus::Start))
            }
            KeyCode::End if snapshot.later() => Some((stdout::Page::End, stdout::Focus::End)),
            KeyCode::Home => {
                popup.y = 0;
                popup.x = 0;
                None
            }
            KeyCode::End => {
                popup.y = last;
                popup.x = 0;
                None
            }
            _ => {
                let anchor = rows.get(popup.y).map(|row| row.key).unwrap_or((0, 0));
                if delta < 0 && popup.y < delta.unsigned_abs() && snapshot.earlier() {
                    Some((stdout::Page::Earlier, stdout::Focus::Move(anchor, delta)))
                } else if delta > 0
                    && popup.y.saturating_add(delta as usize) > last
                    && snapshot.later()
                {
                    Some((stdout::Page::Later, stdout::Focus::Move(anchor, delta)))
                } else {
                    popup.y = popup.y.saturating_add_signed(delta).min(last);
                    None
                }
            }
        };
        if let Some((page, focus)) = request {
            self.page_stdout(page, focus);
        }
    }
    fn key(&mut self, mut key: KeyEvent) -> Option<i32> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            return Some(130);
        }
        if self.edit_search(key) {
            return None;
        }
        if self.help {
            match key.code {
                KeyCode::Char('q' | '?') | KeyCode::Esc | KeyCode::F(1) => self.help = false,
                KeyCode::Down => self.help_y = self.help_y.saturating_add(1),
                KeyCode::Up => self.help_y = self.help_y.saturating_sub(1),
                KeyCode::PageDown => {
                    self.help_y = self
                        .help_y
                        .saturating_add(view::popup_capacity(self.width, self.height))
                }
                KeyCode::PageUp => {
                    self.help_y = self
                        .help_y
                        .saturating_sub(view::popup_capacity(self.width, self.height))
                }
                _ => (),
            }
            return None;
        }
        let stdout_tab = self
            .popup
            .as_ref()
            .is_some_and(|popup| popup.tab == Tab::Stdout);
        if stdout_tab {
            match key.code {
                KeyCode::Char('/') => {
                    self.search_task = None;
                    if let Some(popup) = &mut self.popup {
                        popup.search_edit = Some(String::new());
                        popup.search_message = None;
                    }
                    return None;
                }
                KeyCode::Char('n' | 'N') => {
                    self.start_search(key.code == KeyCode::Char('N'), true);
                    return None;
                }
                KeyCode::Esc if self.search_task.is_some() => {
                    self.search_task = None;
                    if let Some(popup) = &mut self.popup {
                        popup.search_message = Some("Search cancelled".into());
                    }
                    return None;
                }
                _ => (),
            }
            key.code = match key.code {
                KeyCode::Char('g') => KeyCode::Home,
                KeyCode::Char('G') => KeyCode::End,
                KeyCode::Char('j') => KeyCode::Down,
                KeyCode::Char('k') => KeyCode::Up,
                KeyCode::Char('h') => KeyCode::Left,
                KeyCode::Char('l') => KeyCode::Right,
                KeyCode::Char('f') => KeyCode::PageDown,
                KeyCode::Char('b') => KeyCode::PageUp,
                code => code,
            };
        }
        match key.code {
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.help = true;
                self.help_y = 0;
                return None;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.popup.take().is_some() {
                    self.detail_task = None;
                    self.stdout_task = None;
                    self.search_task = None;
                    self.stdout_lines_task = None;
                    return None;
                }
                return Some(0);
            }
            KeyCode::Char(' ') => {
                self.schedule.toggle();
                return None;
            }
            KeyCode::Char('+') => {
                self.schedule.adjust(true);
                return None;
            }
            KeyCode::Char('-') => {
                self.schedule.adjust(false);
                return None;
            }
            KeyCode::Char('r') => {
                if self
                    .popup
                    .as_ref()
                    .is_some_and(|popup| popup.tab == Tab::Stdout)
                {
                    self.refresh_stdout();
                } else if self.popup.is_some() {
                    self.refresh_details();
                } else {
                    self.schedule.refresh();
                }
                return None;
            }
            KeyCode::Char('w') => {
                self.toggle_wrap();
                return None;
            }
            _ => (),
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
            && let Some(popup) = &mut self.popup
        {
            popup.switch_tab(
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT),
            );
            let load = popup.tab == Tab::Stdout && !popup.stdout_loaded;
            self.stdout_task = None;
            self.search_task = None;
            self.stdout_lines_task = None;
            if load {
                self.refresh_stdout();
            }
            return None;
        }
        let page = if self.popup.is_some() {
            view::popup_capacity(self.width, self.height)
        } else {
            view::capacity(self.height)
        };
        if self
            .popup
            .as_ref()
            .is_some_and(|popup| popup.tab == Tab::Stdout && popup.stdout.is_some())
            && matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('d' | 'u')
            )
        {
            self.scroll_stdout(key, page);
            return None;
        }
        if let Some(popup) = &mut self.popup {
            match key.code {
                KeyCode::Up => popup.y = popup.y.saturating_sub(1),
                KeyCode::Down => popup.y = popup.y.saturating_add(1),
                KeyCode::PageUp => popup.y = popup.y.saturating_sub(page),
                KeyCode::PageDown => popup.y = popup.y.saturating_add(page),
                KeyCode::Home => {
                    popup.y = 0;
                    popup.x = 0;
                }
                KeyCode::End => popup.y = usize::MAX,
                KeyCode::Left => {
                    popup.x = popup.x.saturating_sub(if control {
                        self.width.saturating_sub(8)
                    } else {
                        8
                    })
                }
                KeyCode::Right => {
                    popup.x = popup.x.saturating_add(if control {
                        self.width.saturating_sub(8)
                    } else {
                        8
                    })
                }
                _ => (),
            }
            return None;
        }
        match key.code {
            KeyCode::Up => self.selection.move_by(-1, self.rows.len(), page),
            KeyCode::Down => self.selection.move_by(1, self.rows.len(), page),
            KeyCode::PageUp => self
                .selection
                .move_by(-(page as isize), self.rows.len(), page),
            KeyCode::PageDown => self.selection.move_by(page as isize, self.rows.len(), page),
            KeyCode::Home => self.selection.selected = 0,
            KeyCode::End => self.selection.selected = self.rows.len().saturating_sub(1),
            KeyCode::Left => {
                self.selection.horizontal = self.selection.horizontal.saturating_sub(if control {
                    self.width.saturating_sub(3)
                } else {
                    8
                })
            }
            KeyCode::Right => {
                self.selection.horizontal = self.selection.horizontal.saturating_add(if control {
                    self.width.saturating_sub(3)
                } else {
                    8
                })
            }
            KeyCode::Enter => {
                if let Some(row) = self.rows.get(self.selection.selected) {
                    self.popup = Some(Popup::new(
                        row.clone(),
                        self.updated
                            .clone()
                            .unwrap_or_else(|| "not yet updated".into()),
                    ));
                    self.refresh_details();
                }
            }
            _ => (),
        }
        None
    }
}

pub(crate) fn run(setup: Setup) -> Result<i32> {
    if setup.interval.is_zero() || Instant::now().checked_add(setup.interval).is_none() {
        return Err("--tui refresh interval is too large".into());
    }
    let mut screen = Screen::open()?;
    let mut app = App::new(setup)?;
    let mut dirty = true;
    loop {
        let signal = SIGNAL.load(Ordering::Relaxed);
        if signal != 0 {
            return Ok(128 + signal);
        }
        dirty |= app.work()?;
        let (width, height) = crate::terminal::size();
        dirty |= app.width != width || app.height != height;
        if dirty {
            app.reflow(width, height)?;
            screen
                .terminal
                .as_mut()
                .ok_or("terminal unavailable")?
                .draw(|frame| view::draw(frame, &mut app))
                .map_err(|e| format!("draw fullscreen queue: {e}"))?;
            dirty = false;
        }
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => match event::read().map_err(|e| format!("read terminal event: {e}"))? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(code) = app.key(key) {
                        return Ok(code);
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => (),
            },
            Ok(false) => (),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(format!("read terminal: {e}")),
        }
    }
}
