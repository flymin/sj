use crate::queue::clip;
use crate::util::{self, Result};
use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicI32, Ordering};

static INTERRUPTED: AtomicI32 = AtomicI32::new(0);

extern "C" fn interrupted(signal: libc::c_int) {
    INTERRUPTED.store(signal, Ordering::Relaxed);
}

pub fn size() -> (usize, usize) {
    let fallback = |key: &str, default| {
        env::var(key)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(default)
    };
    let mut width = fallback("COLUMNS", 120);
    let mut height = fallback("LINES", 24);
    for fd in [libc::STDOUT_FILENO, libc::STDIN_FILENO] {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // The ioctl writes only this initialized winsize allocation.
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 {
            if ws.ws_col > 0 {
                width = ws.ws_col as usize;
            }
            if ws.ws_row > 0 {
                height = ws.ws_row as usize;
            }
            break;
        }
    }
    (width, height)
}

struct Screen {
    original: libc::termios,
    signals: Vec<(libc::c_int, libc::sigaction)>,
    entered: bool,
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
            original,
            signals: Vec::new(),
            entered: false,
        };
        INTERRUPTED.store(0, Ordering::Relaxed);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut old = unsafe { std::mem::zeroed() };
            action.sa_sigaction = interrupted as *const () as libc::sighandler_t;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
                return Err(format!(
                    "set terminal signal handler: {}",
                    io::Error::last_os_error()
                ));
            }
            screen.signals.push((signal, old));
        }
        let mut raw = original;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        raw.c_oflag = original.c_oflag;
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            return Err(format!(
                "set terminal keyboard mode: {}",
                io::Error::last_os_error()
            ));
        }
        screen.entered = true;
        write("\x1b[?1049h\x1b[?25l")?;
        Ok(screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
        if self.entered {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
            let _ = stdout.flush();
        }
        for (signal, old) in &self.signals {
            unsafe {
                libc::sigaction(*signal, old, std::ptr::null_mut());
            }
        }
    }
}

fn write(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|_| stdout.flush())
        .map_err(|e| format!("draw selection menu: {e}"))
}

#[derive(Debug, Clone, Copy)]
enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Digit(u8),
    Backspace,
    Enter,
    Cancel,
    Ignore,
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Keep,
    Select(usize),
    Cancel,
}

#[derive(Debug, Default)]
struct State {
    selected: usize,
    top: usize,
    number: String,
}

impl State {
    fn visible(&mut self, total: usize, capacity: usize) {
        self.top = self.top.min(total.saturating_sub(capacity));
        if self.selected < self.top {
            self.top = self.selected;
        }
        if self.selected >= self.top + capacity {
            self.top = self.selected + 1 - capacity;
        }
    }

    fn typed_index(&self, total: usize) -> Option<usize> {
        self.number.parse::<usize>().ok().filter(|n| *n < total)
    }

    fn apply(&mut self, key: Key, total: usize, capacity: usize) -> Action {
        match key {
            Key::Cancel => return Action::Cancel,
            Key::Enter => {
                return if self.number.is_empty() {
                    Action::Select(self.selected)
                } else if let Some(index) = self.typed_index(total) {
                    Action::Select(index)
                } else {
                    Action::Keep
                };
            }
            Key::Digit(digit) => {
                if self.number.len() < 20 {
                    self.number.push(digit as char);
                }
                if let Some(index) = self.typed_index(total) {
                    self.selected = index;
                }
            }
            Key::Backspace => {
                self.number.pop();
                if let Some(index) = self.typed_index(total) {
                    self.selected = index;
                }
            }
            Key::Ignore => return Action::Keep,
            other => {
                self.number.clear();
                self.selected = match other {
                    Key::Up => self.selected.saturating_sub(1),
                    Key::Down => (self.selected + 1).min(total - 1),
                    Key::PageUp => self.selected.saturating_sub(capacity),
                    Key::PageDown => self.selected.saturating_add(capacity).min(total - 1),
                    Key::Home => 0,
                    Key::End => total - 1,
                    _ => self.selected,
                };
            }
        }
        self.visible(total, capacity);
        Action::Keep
    }
}

fn capacity(height: usize) -> usize {
    let reserved = usize::from(height >= 3)
        + usize::from(height >= 5)
        + usize::from(height >= 4)
        + usize::from(height >= 2);
    height.saturating_sub(reserved).max(1)
}

fn frame(
    title: &str,
    header: &str,
    items: &[String],
    state: &State,
    dimensions: (usize, usize),
) -> Vec<(String, bool)> {
    let (width, height) = dimensions;
    let count = items.len();
    let mut rows = Vec::new();
    if height >= 3 {
        rows.push((util::clean(title), false));
    }
    if height >= 5 {
        rows.push((format!("  INDEX {header}"), false));
    }
    let end = (state.top + capacity(height)).min(count);
    for (index, item) in items.iter().enumerate().take(end).skip(state.top) {
        let selected = index == state.selected;
        rows.push((
            format!(
                "{} {index:>5} {}",
                if selected { '>' } else { ' ' },
                util::clean(item)
            ),
            selected,
        ));
    }
    if height >= 4 {
        let typed = if state.number.is_empty() {
            String::new()
        } else if state.typed_index(count).is_some() {
            format!(" | index: {}", state.number)
        } else {
            format!(" | invalid index: {} (0..{})", state.number, count - 1)
        };
        rows.push((
            format!(
                "{}-{end}/{count} | selected {}{typed}",
                state.top + 1,
                state.selected
            ),
            false,
        ));
    }
    if height >= 2 {
        rows.push((
            "↑↓ move  0-9 index  Enter select  PgUp/PgDn  q/Esc cancel".into(),
            false,
        ));
    }
    // Reserve the last column so writing a row never causes terminal wrapping.
    for (line, _) in &mut rows {
        *line = clip(line, width.saturating_sub(1).max(1));
    }
    rows
}

fn draw(
    title: &str,
    header: &str,
    items: &[String],
    state: &State,
    dimensions: (usize, usize),
) -> Result<()> {
    let mut text = String::from("\x1b[H\x1b[2J");
    for (row, (line, selected)) in frame(title, header, items, state, dimensions)
        .iter()
        .enumerate()
    {
        text.push_str(&format!("\x1b[{};1H", row + 1));
        if *selected {
            text.push_str("\x1b[7m");
        }
        text.push_str(line);
        if *selected {
            text.push_str("\x1b[0m");
        }
    }
    write(&text)
}

fn read_byte(timeout: i32) -> Result<Option<u8>> {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
    if ready < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(format!("read menu keys: {e}"));
    }
    if ready == 0 {
        return Ok(None);
    }
    if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Ok(Some(4));
    }
    let mut byte = 0_u8;
    let n = unsafe { libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) };
    if n == 1 {
        Ok(Some(byte))
    } else if n == 0 {
        Ok(Some(4))
    } else {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            Ok(None)
        } else {
            Err(format!("read menu key: {e}"))
        }
    }
}

fn key() -> Result<Key> {
    let Some(byte) = read_byte(100)? else {
        return Ok(Key::Ignore);
    };
    Ok(match byte {
        b'0'..=b'9' => Key::Digit(byte),
        b'\r' | b'\n' => Key::Enter,
        8 | 127 => Key::Backspace,
        3 | 4 | b'q' => Key::Cancel,
        27 => {
            let Some(prefix) = read_byte(60)? else {
                return Ok(Key::Cancel);
            };
            if prefix != b'[' && prefix != b'O' {
                return Ok(Key::Cancel);
            }
            let mut sequence = Vec::new();
            while sequence.len() < 16 {
                let Some(next) = read_byte(60)? else {
                    break;
                };
                sequence.push(next);
                if next.is_ascii_alphabetic() || next == b'~' {
                    break;
                }
            }
            match sequence.as_slice() {
                b"A" => Key::Up,
                b"B" => Key::Down,
                b"H" | b"1~" | b"7~" => Key::Home,
                b"F" | b"4~" | b"8~" => Key::End,
                b"5~" => Key::PageUp,
                b"6~" => Key::PageDown,
                _ => Key::Ignore,
            }
        }
        _ => Key::Ignore,
    })
}

pub fn select(title: &str, header: &str, items: &[String]) -> Result<Option<usize>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "selection requires a terminal; supply NODE_INDEX, --host or --container explicitly"
                .into(),
        );
    }
    if items.is_empty() {
        return Ok(None);
    }
    let _screen = Screen::open()?;
    let mut state = State::default();
    let mut dimensions = size();
    draw(title, header, items, &state, dimensions)?;
    loop {
        if INTERRUPTED.load(Ordering::Relaxed) != 0 {
            return Ok(None);
        }
        let pressed = key()?;
        let changed = size();
        let resize = changed != dimensions;
        dimensions = changed;
        state.visible(items.len(), capacity(dimensions.1));
        match state.apply(pressed, items.len(), capacity(dimensions.1)) {
            Action::Select(index) => return Ok(Some(index)),
            Action::Cancel => return Ok(None),
            Action::Keep => (),
        }
        if resize || !matches!(pressed, Key::Ignore) {
            draw(title, header, items, &state, dimensions)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_and_long_lists_stay_inside_terminal() {
        for height in 1..=40 {
            for total in [1, 3, 32, 128] {
                let items: Vec<_> = (0..total)
                    .map(|n| format!("node-{n}-very-long-hostname-训练"))
                    .collect();
                let mut state = State::default();
                for selected in 0..total {
                    state.selected = selected;
                    state.visible(total, capacity(height));
                    let rows = frame("Select node", "HOSTNAME", &items, &state, (30, height));
                    assert!(rows.len() <= height);
                    assert_eq!(rows.iter().filter(|(_, selected)| *selected).count(), 1);
                    assert!(
                        rows.iter()
                            .all(|(s, _)| unicode_width::UnicodeWidthStr::width(s.as_str()) <= 29)
                    );
                }
            }
        }
        let items = vec!["a".into(), "b".into(), "host shell".into()];
        let rows = frame("Choose", "TARGET", &items, &State::default(), (80, 24));
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().any(|(s, _)| s.contains("host shell")));
    }
    #[test]
    fn arrows_numbers_and_pages_select_the_highlight() {
        let mut state = State::default();
        assert_eq!(state.apply(Key::Enter, 32, 4), Action::Select(0));
        state.apply(Key::Down, 32, 4);
        assert_eq!(state.apply(Key::Enter, 32, 4), Action::Select(1));
        state.apply(Key::Digit(b'1'), 32, 4);
        state.apply(Key::Digit(b'2'), 32, 4);
        assert_eq!(state.apply(Key::Enter, 32, 4), Action::Select(12));
        assert!(state.top <= 12 && state.top + 4 > 12);
        state.apply(Key::End, 32, 4);
        assert_eq!(state.selected, 31);
        state.apply(Key::PageUp, 32, 4);
        assert_eq!(state.selected, 27);
        state.apply(Key::Home, 32, 4);
        assert_eq!(state.selected, 0);
        state.apply(Key::Digit(b'9'), 32, 4);
        state.apply(Key::Digit(b'9'), 32, 4);
        assert_eq!(state.apply(Key::Enter, 32, 4), Action::Keep);
        state.apply(Key::Backspace, 32, 4);
        assert_eq!(state.apply(Key::Enter, 32, 4), Action::Select(9));
        assert_eq!(state.apply(Key::Cancel, 32, 4), Action::Cancel);
    }
}
