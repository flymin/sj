mod details;
mod model;
mod process;
mod view;

use crate::{
    config::{Config, Resolved},
    fields::{Catalog, Row},
    queue::{Query, update_timestamp},
    util::Result,
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use details::{Controller, Popup};
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
    fn key(&mut self, key: KeyEvent) -> Option<i32> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            return Some(130);
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
        match key.code {
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.help = true;
                self.help_y = 0;
                return None;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.popup.take().is_some() {
                    self.detail_task = None;
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
                if self.popup.is_some() {
                    self.refresh_details();
                } else {
                    self.schedule.refresh();
                }
                return None;
            }
            _ => (),
        }
        let page = if self.popup.is_some() {
            view::popup_capacity(self.width, self.height)
        } else {
            view::capacity(self.height)
        };
        if let Some(popup) = &mut self.popup {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    popup.raw = !popup.raw;
                    popup.x = 0;
                    popup.y = 0;
                }
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
