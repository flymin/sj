use super::{details, process};
mod ansi;
use crate::{fields::Row as Job, logs, queue::update_timestamp, util::Result};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, TryLockError, atomic::AtomicBool},
    time::Instant,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// Cache windows, not limits on the file or the history available for browsing.
const BLOCK_BYTES: u64 = 8192;
const CACHE_BLOCKS: u64 = 3;
const MAX_CACHE_BLOCKS: usize = 512;
// Fixed-size sparse checkpoints keep line metadata bounded even for huge logs.
const LINE_CHECKPOINTS: u64 = 4096;
pub type Anchor = (u64, usize);

#[derive(Clone, Copy)]
pub enum Page {
    Start,
    End,
    Earlier,
    Later,
    At(u64),
}
#[derive(Clone, Copy)]
pub enum Focus {
    Start,
    End,
    Move(Anchor, isize),
    Horizontal(Anchor, isize),
    Found(Anchor),
}
pub struct DisplayRow {
    pub key: Anchor,
    pub text: String,
    spans: Vec<Span>,
    pub marks: Vec<(usize, usize)>,
    text_start: usize,
}
struct Span {
    block: u64,
    start: usize,
    end: usize,
    row_start: usize,
}
impl DisplayRow {
    pub fn contains(&self, anchor: Anchor) -> bool {
        self.spans.iter().any(|span| {
            span.block == anchor.0
                && anchor.1 >= span.start
                && (anchor.1 < span.end || span.start == span.end && anchor.1 == span.start)
        })
    }
    pub fn column(&self, anchor: Anchor) -> Option<usize> {
        self.spans
            .iter()
            .find(|span| {
                span.block == anchor.0
                    && anchor.1 >= span.start
                    && (anchor.1 < span.end || span.start == span.end && anchor.1 == span.start)
            })
            .map(|span| {
                UnicodeWidthStr::width(&self.text[..span.row_start + anchor.1 - span.start])
            })
    }
    pub fn at_column(&self, column: usize) -> Anchor {
        let mut used = 0;
        let mut position = 0;
        for (index, glyph) in self.text.grapheme_indices(true) {
            let size = UnicodeWidthStr::width(glyph);
            if used + size > column {
                position = index;
                break;
            }
            used += size;
            position = index + glyph.len();
        }
        if position == self.text.len() {
            return self
                .spans
                .last()
                .map_or(self.key, |span| (span.block, span.end));
        }
        self.spans
            .iter()
            .find(|span| {
                position >= span.row_start
                    && (position < span.row_start + span.end - span.start || span.start == span.end)
            })
            .map(|span| (span.block, span.start + position - span.row_start))
            .unwrap_or(self.key)
    }
}
struct Source {
    file: File,
    size: u64,
    lines: OnceLock<LineIndex>,
    ansi: Mutex<ansi::Index>,
    #[cfg(test)]
    hole_queries: std::sync::atomic::AtomicUsize,
}
struct LineIndex {
    checkpoints: Vec<(u64, u64)>,
    total: u64,
}
struct Block {
    index: u64,
    start: u64,
    end: u64,
    text: String,
    starts_line: bool,
}

#[derive(Clone)]
pub struct Snapshot {
    pub path: PathBuf,
    pub updated: String,
    source: Arc<Source>,
    blocks: Vec<Arc<Block>>,
    line_start: Option<u64>,
    rows_hint: usize,
}

fn check(cancel: &AtomicBool, deadline: Instant) -> Result<()> {
    if process::cancelled(cancel) {
        Err("stdout load cancelled".into())
    } else if Instant::now() >= deadline {
        Err("stdout read timed out".into())
    } else {
        Ok(())
    }
}

impl Source {
    fn hole_end(&self, index: u64, limit: u64, cancel: &AtomicBool) -> Result<Option<u64>> {
        use std::os::fd::AsRawFd;
        check(cancel, Instant::now() + process::TIMEOUT)?;
        #[cfg(test)]
        {
            self.hole_queries
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let data = unsafe {
            libc::lseek(
                self.file.as_raw_fd(),
                (index * BLOCK_BYTES) as libc::off_t,
                libc::SEEK_DATA,
            )
        };
        let data = if data >= 0 {
            data as u64
        } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
            self.size
        } else {
            return Ok(None);
        };
        // Keep the block preceding data: boundary() can extend through its first newline.
        let end = (data / BLOCK_BYTES).saturating_sub(1).min(limit);
        Ok((end > index).then_some(end))
    }
    fn parser(&self, cancel: &AtomicBool) -> Result<std::sync::MutexGuard<'_, ansi::Index>> {
        loop {
            check(cancel, Instant::now() + process::TIMEOUT)?;
            match self.ansi.try_lock() {
                Ok(parser) => return Ok(parser),
                Err(TryLockError::Poisoned(_)) => return Err("stdout parser stopped".into()),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
            }
        }
    }
    fn count(&self) -> u64 {
        self.size.div_ceil(BLOCK_BYTES).max(1)
    }
    fn bytes(
        &self,
        start: u64,
        end: u64,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<Vec<u8>> {
        let mut bytes = vec![0; (end - start) as usize];
        let mut done = 0;
        while done < bytes.len() {
            check(cancel, deadline)?;
            let count = self
                .file
                .read_at(&mut bytes[done..], start + done as u64)
                .map_err(|error| format!("read stdout: {error}"))?;
            if count == 0 {
                return Err("Log was truncated; press r to reload".into());
            }
            done += count;
        }
        Ok(bytes)
    }
    fn boundary(&self, index: u64, cancel: &AtomicBool, deadline: Instant) -> Result<u64> {
        let start = index * BLOCK_BYTES;
        if index == 0 || start >= self.size {
            return Ok(start.min(self.size));
        }
        let bytes = self.bytes(
            start,
            (start + BLOCK_BYTES + 3).min(self.size),
            cancel,
            deadline,
        )?;
        if let Some(i) = bytes
            .iter()
            .take(BLOCK_BYTES as usize)
            .position(|byte| matches!(byte, b'\n' | b'\r'))
        {
            let crlf = bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n');
            return Ok(start + i as u64 + if crlf { 2 } else { 1 });
        }
        // Very long records can span windows; never split a UTF-8 code point.
        let skip = bytes
            .iter()
            .take(3)
            .take_while(|byte| **byte & 0xc0 == 0x80)
            .count();
        Ok(start + skip as u64)
    }
}

impl Snapshot {
    pub fn compact(&self, focus: Focus) -> Self {
        let middle = match focus {
            Focus::Start => 0,
            Focus::End => self.blocks.len() - 1,
            Focus::Move(anchor, _) | Focus::Horizontal(anchor, _) | Focus::Found(anchor) => self
                .blocks
                .iter()
                .position(|block| block.index == anchor.0)
                .unwrap_or(0),
        };
        let first = middle
            .saturating_sub(1)
            .min(self.blocks.len().saturating_sub(CACHE_BLOCKS as usize));
        let end = (first + CACHE_BLOCKS as usize).min(self.blocks.len());
        let skipped = self.blocks[..first]
            .iter()
            .map(|block| block.text.bytes().filter(|&byte| byte == b'\n').count() as u64)
            .sum::<u64>();
        Self {
            blocks: self.blocks[first..end].to_vec(),
            line_start: self.line_start.map(|line| line + skipped),
            rows_hint: 0,
            ..self.clone()
        }
    }
    pub fn needs_rows(&self, capacity: usize) -> bool {
        self.rows_hint < capacity && (self.earlier() || self.later())
    }
    pub fn needs_lines(&self) -> bool {
        self.line_start.is_none()
    }
    pub fn merge_lines(&mut self, counted: &Self) {
        if Arc::ptr_eq(&self.source, &counted.source)
            && self.blocks[0].index == counted.blocks[0].index
        {
            self.line_start = counted.line_start;
        }
    }
    pub fn line_position(&self, rows: &[DisplayRow], top: usize) -> Option<(u64, u64)> {
        let total = self.source.lines.get()?.total;
        let mut remaining = rows.get(top).map_or(0, |row| row.text_start);
        let mut current = self.line_start?;
        for block in &self.blocks {
            let length = remaining.min(block.text.len());
            current += block.text.as_bytes()[..length]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count() as u64;
            remaining -= length;
        }
        Some(((current + 1).min(total), total))
    }
    pub fn earlier(&self) -> bool {
        self.blocks[0].index > 0
    }
    pub fn later(&self) -> bool {
        self.blocks.last().unwrap().index + 1 < self.source.count()
    }
    pub fn continues_before(&self) -> bool {
        self.earlier() && !self.blocks[0].starts_line
    }
    pub fn continues_after(&self) -> bool {
        self.later() && !self.blocks.last().unwrap().text.ends_with('\n')
    }
    pub fn rows(&self, width: usize, wrap: bool) -> Vec<DisplayRow> {
        self.marked_rows(width, wrap, None)
    }
    pub fn marked_rows(&self, width: usize, wrap: bool, pattern: Option<&str>) -> Vec<DisplayRow> {
        self.build_rows(width, wrap, pattern, None)
    }
    pub fn visible_rows(
        &self,
        width: usize,
        wrap: bool,
        pattern: Option<&str>,
        top: usize,
        capacity: usize,
        horizontal: usize,
    ) -> Vec<DisplayRow> {
        self.build_rows(width, wrap, pattern, Some((top, capacity, horizontal)))
    }
    fn build_rows(
        &self,
        width: usize,
        wrap: bool,
        pattern: Option<&str>,
        visible: Option<(usize, usize, usize)>,
    ) -> Vec<DisplayRow> {
        let width = width.max(1);
        let mut text = String::new();
        let mut ranges = vec![];
        for block in &self.blocks {
            let start = text.len();
            text.push_str(&block.text);
            ranges.push((block.index, start, text.len()));
        }
        let row = |start: usize, end: usize| {
            let spans: Vec<_> = ranges
                .iter()
                .filter_map(|&(block, first, last)| {
                    let low = start.max(first);
                    let high = end.min(last);
                    if low < high || start == end && first <= start && start < last {
                        Some(Span {
                            block,
                            start: low - first,
                            end: high - first,
                            row_start: low - start,
                        })
                    } else {
                        None
                    }
                })
                .collect();
            let key = spans
                .first()
                .map(|span| (span.block, span.start))
                .unwrap_or((0, 0));
            DisplayRow {
                key,
                text: text[start..end].into(),
                spans,
                marks: vec![],
                text_start: start,
            }
        };
        let mut rows = Vec::new();
        let mut position = 0;
        for record in text.split_inclusive('\n') {
            let line = record.strip_suffix('\n').unwrap_or(record);
            let mut start = position;
            if wrap {
                let mut used = 0;
                for (index, glyph) in line.grapheme_indices(true) {
                    let size = UnicodeWidthStr::width(glyph);
                    if used > 0 && used + size > width {
                        rows.push(row(start, position + index));
                        start = position + index;
                        used = 0;
                    }
                    used += size;
                }
            }
            rows.push(row(start, position + line.len()));
            position += record.len();
        }
        if let Some(pattern) = pattern.filter(|pattern| !pattern.is_empty()) {
            if let Some((top, capacity, horizontal)) = visible {
                let top = top.min(rows.len().saturating_sub(capacity));
                for row in rows.iter_mut().skip(top).take(capacity) {
                    let mut columns = 0;
                    let mut low = None;
                    let mut high = 0;
                    for (index, glyph) in row.text.grapheme_indices(true) {
                        if columns >= horizontal.saturating_add(width) {
                            break;
                        }
                        let next = columns + UnicodeWidthStr::width(glyph);
                        if next > horizontal {
                            low.get_or_insert(index);
                            high = index + glyph.len();
                        }
                        columns = next;
                    }
                    let Some(low) = low else {
                        continue;
                    };
                    let start = row.text_start + low;
                    let end = row.text_start + high;
                    let context = pattern.len().saturating_sub(1);
                    let first = start.saturating_sub(context);
                    let last = end.saturating_add(context).min(text.len());
                    let mut matcher = Matcher::new(pattern, false);
                    for (offset, &byte) in text.as_bytes()[first..last].iter().enumerate() {
                        if let Some((_, found)) = matcher.feed(byte, (0, first + offset), false) {
                            let low = found.max(start) - row.text_start;
                            let high = (found + pattern.len())
                                .min(end)
                                .saturating_sub(row.text_start);
                            if low < high {
                                if let Some(previous) =
                                    row.marks.last_mut().filter(|previous| low <= previous.1)
                                {
                                    previous.1 = previous.1.max(high);
                                } else {
                                    row.marks.push((low, high));
                                }
                            }
                        }
                    }
                }
                return rows;
            }
            let mut first = 0;
            let mut matcher = Matcher::new(pattern, false);
            for (index, byte) in text.bytes().enumerate() {
                let Some((_, start)) = matcher.feed(byte, (0, index), false) else {
                    continue;
                };
                let end = start + pattern.len();
                while first < rows.len() && rows[first].text_start + rows[first].text.len() <= start
                {
                    first += 1;
                }
                for row in rows[first..]
                    .iter_mut()
                    .take_while(|row| row.text_start < end)
                {
                    let low = start.max(row.text_start);
                    let high = end.min(row.text_start + row.text.len());
                    if low < high {
                        row.marks
                            .push((low - row.text_start, high - row.text_start));
                    }
                }
            }
        }
        rows
    }
    pub fn focus(&self, focus: Focus, width: usize, capacity: usize, wrap: bool) -> usize {
        let rows = self.rows(width, wrap);
        let last = rows.len().saturating_sub(capacity);
        match focus {
            Focus::Start => 0,
            Focus::End => last,
            Focus::Move(anchor, delta) => rows
                .iter()
                .position(|row| row.contains(anchor))
                .map(|index| index.saturating_add_signed(delta).min(last))
                .unwrap_or(if delta < 0 { last } else { 0 }),
            Focus::Horizontal(anchor, _) | Focus::Found(anchor) => rows
                .iter()
                .position(|row| row.contains(anchor))
                .unwrap_or(0)
                .min(last),
        }
    }
    pub fn horizontal(&self, focus: Focus, width: usize, wrap: bool) -> usize {
        if let Focus::Found(anchor) = focus
            && !wrap
        {
            return self
                .rows(width, false)
                .iter()
                .find_map(|row| row.column(anchor))
                .unwrap_or(0)
                .saturating_sub(width / 3);
        }
        if let Focus::Horizontal(anchor, delta) = focus
            && !wrap
        {
            return self
                .rows(width, false)
                .iter()
                .find_map(|row| row.column(anchor))
                .unwrap_or(0)
                .saturating_add_signed(delta);
        }
        0
    }
    pub fn progress(&self, rows: &[DisplayRow], top: usize, capacity: usize) -> usize {
        if self.source.size == 0 || (!self.later() && top >= rows.len().saturating_sub(capacity)) {
            return 100;
        }
        let Some(row) = rows.get(top) else {
            return 0;
        };
        let block = self
            .blocks
            .iter()
            .find(|block| block.index == row.key.0)
            .unwrap();
        let offset = block.start as u128
            + (block.end - block.start) as u128 * row.key.1 as u128
                / block.text.len().max(1) as u128;
        (offset * 100 / self.source.size as u128).min(100) as usize
    }
}

pub fn fetch(row: &Job, capacity: usize, cancel: &AtomicBool) -> Result<Snapshot> {
    let (cluster, id) = details::task_id(row)?;
    let job = id.split('.').next().unwrap_or(&id);
    let path = logs::resolve(job, false, &cluster, |command| {
        command.env("LC_ALL", "C");
        process::capture(command, cancel, process::TIMEOUT)
    })?;
    let mut snapshot = open(&path, cancel)?;
    snapshot.rows_hint = capacity;
    navigate(&snapshot, Page::End, cancel)
}

pub fn resize(current: &Snapshot, capacity: usize, cancel: &AtomicBool) -> Result<Snapshot> {
    let mut snapshot = current.clone();
    snapshot.rows_hint = capacity;
    let page = if current.later() {
        Page::At(current.blocks[0].index + 1)
    } else {
        Page::End
    };
    navigate(&snapshot, page, cancel)
}

fn open(path: &Path, cancel: &AtomicBool) -> Result<Snapshot> {
    let deadline = Instant::now() + process::TIMEOUT;
    check(cancel, deadline)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            format!(
                "cannot open {}: {error}; check that the log exists and is mounted here",
                path.display()
            )
        })?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular log file", path.display()));
    }
    let snapshot = Snapshot {
        path: path.into(),
        updated: update_timestamp(),
        source: Arc::new(Source {
            file,
            size: metadata.len(),
            lines: OnceLock::new(),
            ansi: Mutex::new(ansi::Index::new(metadata.len())),
            #[cfg(test)]
            hole_queries: std::sync::atomic::AtomicUsize::new(0),
        }),
        blocks: vec![],
        line_start: None,
        rows_hint: 0,
    };
    snapshot
        .source
        .parser(cancel)?
        .prepare(&snapshot.source, cancel)?;
    navigate(&snapshot, Page::End, cancel)
}

pub fn navigate(current: &Snapshot, page: Page, cancel: &AtomicBool) -> Result<Snapshot> {
    navigate_rows(current, page, 1, cancel)
}

pub fn move_page(
    current: &Snapshot,
    page: Page,
    focus: Focus,
    cancel: &AtomicBool,
) -> Result<Snapshot> {
    if current.rows_hint > 0
        && matches!(page, Page::Earlier | Page::Later)
        && let Focus::Move(anchor, delta) = focus
        && let Some(blocks) = moving_blocks(current, anchor, delta, cancel)?
    {
        return with_blocks(current, blocks, cancel);
    }
    let rows = if let Focus::Move(_, delta) = focus {
        delta.unsigned_abs().max(1)
    } else {
        1
    };
    navigate_rows(current, page, rows, cancel)
}

fn navigate_rows(
    current: &Snapshot,
    page: Page,
    shift: usize,
    cancel: &AtomicBool,
) -> Result<Snapshot> {
    let deadline = Instant::now() + process::TIMEOUT;
    check(cancel, deadline)?;
    let last_start = current.source.count().saturating_sub(CACHE_BLOCKS);
    let first = match page {
        Page::Start => 0,
        Page::End => last_start,
        Page::Earlier => current.blocks[0].index.saturating_sub(1),
        Page::Later => (current.blocks[0].index + 1).min(last_start),
        Page::At(index) => index.saturating_sub(1).min(last_start),
    };
    let end = (first + CACHE_BLOCKS).min(current.source.count());
    let blocks = if let Some(blocks) = viewport_blocks(current, page, shift, cancel, deadline)? {
        blocks
    } else {
        let mut blocks = vec![];
        for index in first..end {
            blocks.push(load_block(current, index, cancel, deadline)?);
        }
        blocks
    };
    check(cancel, deadline)?;
    with_blocks(current, blocks, cancel)
}

fn with_blocks(
    current: &Snapshot,
    blocks: Vec<Arc<Block>>,
    cancel: &AtomicBool,
) -> Result<Snapshot> {
    let mut snapshot = Snapshot {
        blocks,
        line_start: None,
        ..current.clone()
    };
    if let Some(index) = current.source.lines.get() {
        snapshot.line_start = Some(line_start(&snapshot, index, cancel)?);
    }
    Ok(snapshot)
}

fn moving_blocks(
    current: &Snapshot,
    anchor: Anchor,
    delta: isize,
    cancel: &AtomicBool,
) -> Result<Option<Vec<Arc<Block>>>> {
    let deadline = Instant::now() + process::TIMEOUT;
    let mut first = anchor.0.min(current.source.count() - 1);
    let mut end = first + 1;
    let block = load_block(current, first, cancel, deadline)?;
    let offset = anchor.1.min(block.text.len());
    let mut before = block.text.as_bytes()[..offset]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count();
    let mut after = block.text.as_bytes()[offset..]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count();
    let before_target = if delta < 0 { delta.unsigned_abs() } else { 0 }.saturating_add(2);
    let after_target = current
        .rows_hint
        .saturating_add(if delta > 0 { delta as usize } else { 0 })
        .saturating_add(2);
    let mut blocks = VecDeque::from([block]);
    while first > 0
        && blocks.len() < MAX_CACHE_BLOCKS
        && (before < before_target || !blocks.front().unwrap().starts_line)
    {
        first -= 1;
        let block = load_block(current, first, cancel, deadline)?;
        before += block.text.bytes().filter(|&byte| byte == b'\n').count();
        blocks.push_front(block);
    }
    while end < current.source.count() && blocks.len() < MAX_CACHE_BLOCKS && after < after_target {
        let block = load_block(current, end, cancel, deadline)?;
        after += block.text.bytes().filter(|&byte| byte == b'\n').count();
        blocks.push_back(block);
        end += 1;
    }
    if blocks.len() >= MAX_CACHE_BLOCKS && (before < before_target || after < after_target) {
        return Ok(None);
    }
    if before + after <= 1 && (before + after == 0 || first > 0 || end < current.source.count()) {
        return Ok(None);
    }
    Ok(Some(blocks.into()))
}

fn viewport_blocks(
    current: &Snapshot,
    page: Page,
    shift: usize,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Option<Vec<Arc<Block>>>> {
    if current.rows_hint == 0 {
        return Ok(None);
    }
    let count = current.source.count();
    let last = count - 1;
    let target = current.rows_hint.saturating_add(2);
    let backwards = matches!(page, Page::End);
    let mut first = match page {
        Page::Start => 0,
        Page::End => count.saturating_sub(CACHE_BLOCKS),
        Page::Earlier => current.blocks[0].index.saturating_sub(1),
        Page::Later => (current.blocks[0].index + 1).min(last),
        Page::At(index) => index.saturating_sub(1).min(last),
    };
    if !backwards {
        let original = first;
        let later = matches!(page, Page::Later);
        let earlier = matches!(page, Page::Earlier);
        let mut skipped = if later {
            current.blocks[0]
                .text
                .bytes()
                .filter(|&byte| byte == b'\n')
                .count()
        } else {
            0
        };
        for _ in 0..MAX_CACHE_BLOCKS {
            let block = load_block(current, first, cancel, deadline)?;
            if earlier {
                skipped += block.text.bytes().filter(|&byte| byte == b'\n').count();
            }
            if block.starts_line && (!(earlier || later) || skipped >= shift || first == 0) {
                break;
            }
            if later && first < last {
                skipped += block.text.bytes().filter(|&byte| byte == b'\n').count();
                first += 1;
            } else if !later && first > 0 {
                first -= 1;
            } else {
                first = original;
                break;
            }
        }
        if !load_block(current, first, cancel, deadline)?.starts_line {
            return Ok(None);
        }
    }
    let mut blocks = VecDeque::new();
    let mut end = if backwards { count } else { first };
    let mut lines = 0;
    loop {
        if backwards {
            if blocks.is_empty() {
                for index in first..end {
                    let block = load_block(current, index, cancel, deadline)?;
                    lines += block.text.bytes().filter(|&byte| byte == b'\n').count();
                    blocks.push_back(block);
                }
            } else {
                if first == 0 {
                    break;
                }
                first -= 1;
                let block = load_block(current, first, cancel, deadline)?;
                lines += block.text.bytes().filter(|&byte| byte == b'\n').count();
                blocks.push_front(block);
            }
        } else {
            if end == count {
                break;
            }
            let block = load_block(current, end, cancel, deadline)?;
            lines += block.text.bytes().filter(|&byte| byte == b'\n').count();
            blocks.push_back(block);
            end += 1;
        }
        if blocks.len() >= MAX_CACHE_BLOCKS
            || blocks.len() >= CACHE_BLOCKS as usize
                && lines >= target
                && (!backwards || blocks.front().unwrap().starts_line)
        {
            break;
        }
    }
    // A single giant record still uses byte windows for horizontal navigation.
    if blocks.len() >= MAX_CACHE_BLOCKS && lines < target
        || lines <= 1 && (lines == 0 || first > 0 || end < count)
    {
        return Ok(None);
    }
    Ok(Some(blocks.into()))
}

fn line_start(current: &Snapshot, index: &LineIndex, cancel: &AtomicBool) -> Result<u64> {
    let first = current.blocks[0].index;
    let point = index
        .checkpoints
        .partition_point(|&(block, _)| block <= first)
        - 1;
    let (start, mut lines) = index.checkpoints[point];
    let mut block = start;
    while block < first {
        if (block == start || block % 8 == 0)
            && let Some(end) = current.source.hole_end(block, first, cancel)?
        {
            block = end;
            continue;
        }
        let chunk = load_block(current, block, cancel, Instant::now() + process::TIMEOUT)?;
        lines += chunk.text.bytes().filter(|&byte| byte == b'\n').count() as u64;
        block += 1;
    }
    Ok(lines)
}

pub fn count_lines(current: &Snapshot, cancel: &AtomicBool) -> Result<Snapshot> {
    check(cancel, Instant::now() + process::TIMEOUT)?;
    if current.source.lines.get().is_none() {
        let stride = current.source.count().div_ceil(LINE_CHECKPOINTS);
        let mut checkpoints = vec![];
        let mut lines = 0;
        let mut last = None;
        let mut index = 0;
        while index < current.source.count() {
            if index % stride == 0 {
                checkpoints.push((index, lines));
            }
            if index % 8 == 0
                && let Some(end) = current
                    .source
                    .hole_end(index, current.source.count(), cancel)?
            {
                let mut point = (index / stride + 1) * stride;
                while point < end {
                    checkpoints.push((point, lines));
                    point += stride;
                }
                index = end;
                continue;
            }
            let block = load_block(current, index, cancel, Instant::now() + process::TIMEOUT)?;
            lines += block.text.bytes().filter(|&byte| byte == b'\n').count() as u64;
            last = block.text.bytes().last().or(last);
            index += 1;
        }
        check(cancel, Instant::now() + process::TIMEOUT)?;
        let _ = current.source.lines.set(LineIndex {
            checkpoints,
            total: lines + u64::from(last.is_some_and(|byte| byte != b'\n')),
        });
    }
    let mut counted = current.clone();
    counted.line_start = Some(line_start(
        current,
        current.source.lines.get().unwrap(),
        cancel,
    )?);
    Ok(counted)
}

fn load_block(
    current: &Snapshot,
    index: u64,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Arc<Block>> {
    check(cancel, deadline)?;
    if let Some(block) = current.blocks.iter().find(|block| block.index == index) {
        return Ok(block.clone());
    }
    let start = current.source.boundary(index, cancel, deadline)?;
    let end = current.source.boundary(index + 1, cancel, deadline)?;
    let bytes = current.source.bytes(start, end, cancel, deadline)?;
    let starts_line = start == 0
        || current
            .source
            .bytes(start - 1, start, cancel, deadline)?
            .first()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'));
    let bytes = current
        .source
        .parser(cancel)?
        .text(&current.source, start, &bytes, cancel)?;
    let text = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ");
    let text = text
        .chars()
        .map(|ch| {
            if ch.is_control() && ch != '\n' {
                ' '
            } else {
                ch
            }
        })
        .collect();
    Ok(Arc::new(Block {
        index,
        start,
        end,
        text,
        starts_line,
    }))
}

pub enum SearchResult {
    Found {
        snapshot: Snapshot,
        anchor: Anchor,
        wrapped: bool,
    },
    NotFound,
}

// Streaming literal matching carries only pattern-sized state across file windows.
struct Matcher {
    bytes: Vec<u8>,
    prefix: Vec<usize>,
    matched: usize,
    recent: std::collections::VecDeque<Anchor>,
}
impl Matcher {
    fn new(pattern: &str, backwards: bool) -> Self {
        let mut bytes = pattern.as_bytes().to_vec();
        if backwards {
            bytes.reverse();
        }
        let mut prefix = vec![0; bytes.len()];
        for i in 1..bytes.len() {
            let mut j = prefix[i - 1];
            while j > 0 && bytes[i] != bytes[j] {
                j = prefix[j - 1];
            }
            if bytes[i] == bytes[j] {
                j += 1;
            }
            prefix[i] = j;
        }
        Self {
            bytes,
            prefix,
            matched: 0,
            recent: std::collections::VecDeque::new(),
        }
    }
    fn feed(&mut self, byte: u8, anchor: Anchor, backwards: bool) -> Option<Anchor> {
        self.recent.push_back(anchor);
        if self.recent.len() > self.bytes.len() {
            self.recent.pop_front();
        }
        while self.matched > 0 && self.bytes[self.matched] != byte {
            self.matched = self.prefix[self.matched - 1];
        }
        if self.bytes[self.matched] == byte {
            self.matched += 1;
        }
        if self.matched == self.bytes.len() {
            self.matched = self.prefix[self.matched - 1];
            Some(if backwards {
                anchor
            } else {
                *self.recent.front().unwrap()
            })
        } else {
            None
        }
    }
}

fn scan(
    current: &Snapshot,
    pattern: &str,
    range: std::ops::RangeInclusive<u64>,
    backwards: bool,
    origin: Option<(Anchor, bool)>,
    cancel: &AtomicBool,
) -> Result<Option<Anchor>> {
    let mut matcher = Matcher::new(pattern, backwards);
    let mut index = if backwards {
        *range.end()
    } else {
        *range.start()
    };
    loop {
        let deadline = Instant::now() + process::TIMEOUT;
        let block = load_block(current, index, cancel, deadline)?;
        for step in 0..block.text.len() {
            if step % 4096 == 0 {
                check(cancel, deadline)?;
            }
            let offset = if backwards {
                block.text.len() - 1 - step
            } else {
                step
            };
            let anchor = (index, offset);
            if !backwards
                && origin.is_some_and(|(from, skip)| anchor < from || skip && anchor == from)
            {
                continue;
            }
            if let Some(found) = matcher.feed(block.text.as_bytes()[offset], anchor, backwards)
                && (!backwards
                    || origin.is_none_or(|(from, skip)| found < from || !skip && found == from))
            {
                return Ok(Some(found));
            }
        }
        if backwards {
            if index == *range.start() {
                break;
            }
            index -= 1;
        } else {
            if index == *range.end() {
                break;
            }
            index += 1;
        }
    }
    Ok(None)
}

pub fn search(
    current: &Snapshot,
    pattern: &str,
    origin: Anchor,
    backwards: bool,
    skip: bool,
    wrap: bool,
    cancel: &AtomicBool,
) -> Result<SearchResult> {
    if pattern.is_empty() {
        return Err("Search text is empty".into());
    }
    let last = current.source.count() - 1;
    let mut end = origin.0.min(last);
    if backwards {
        let mut available = 0;
        loop {
            let block = load_block(current, end, cancel, Instant::now() + process::TIMEOUT)?;
            available += if end == origin.0 {
                block.text.len().saturating_sub(origin.1)
            } else {
                block.text.len()
            };
            if available >= pattern.len() || end == last {
                break;
            }
            end += 1;
        }
    }
    let range = if backwards {
        0..=end
    } else {
        origin.0.min(last)..=last
    };
    let (anchor, wrapped) = if let Some(anchor) = scan(
        current,
        pattern,
        range,
        backwards,
        Some((origin, skip)),
        cancel,
    )? {
        (Some(anchor), false)
    } else if !wrap || (!backwards && origin == (0, 0) && !skip) {
        (None, false)
    } else {
        (
            scan(current, pattern, 0..=last, backwards, None, cancel)?,
            true,
        )
    };
    match anchor {
        Some(anchor) => Ok(SearchResult::Found {
            snapshot: navigate(current, Page::At(anchor.0), cancel)?,
            anchor,
            wrapped,
        }),
        None => Ok(SearchResult::NotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::DirBuilderExt,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sj-stdout-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("job.out")
        }
        fn read(&self) -> Snapshot {
            open(&self.path(), &AtomicBool::new(false)).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn normalizes_progress_and_terminal_codes_and_handles_empty_and_invalid_utf8() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path(),
            b"first\r\n\x1b[31mcolored\x1b[0m\rprogress\t42\nbad:\xff\n",
        )
        .unwrap();
        assert_eq!(
            fixture
                .read()
                .rows(80, true)
                .into_iter()
                .map(|row| row.text)
                .collect::<Vec<_>>(),
            ["first", "colored", "progress    42", "bad:\u{fffd}"]
        );
        fs::write(fixture.path(), "").unwrap();
        assert!(fixture.read().rows(80, true).is_empty());
    }
    #[test]
    fn browses_all_history_without_a_line_or_file_size_limit() {
        let fixture = Fixture::new();
        let text = (0..20000)
            .map(|i| format!("line-{i:05}\n"))
            .collect::<String>();
        fs::write(fixture.path(), &text).unwrap();
        let mut page = fixture.read();
        assert_eq!(page.rows(80, true).last().unwrap().text, "line-19999");
        let mut all = std::collections::BTreeMap::new();
        loop {
            for block in &page.blocks {
                all.insert(block.index, block.text.clone());
            }
            assert!(page.blocks.len() <= CACHE_BLOCKS as usize);
            if !page.earlier() {
                break;
            }
            page = navigate(&page, Page::Earlier, &AtomicBool::new(false)).unwrap();
        }
        assert_eq!(all.into_values().collect::<String>(), text);
        assert_eq!(page.rows(80, true)[0].text, "line-00000");
        while page.later() {
            page = navigate(&page, Page::Later, &AtomicBool::new(false)).unwrap();
        }
        assert_eq!(page.rows(80, true).last().unwrap().text, "line-19999");
    }
    #[test]
    fn long_utf8_records_remain_complete_across_windows() {
        let fixture = Fixture::new();
        let text = "训练".repeat(20000);
        fs::write(fixture.path(), &text).unwrap();
        let mut page = navigate(&fixture.read(), Page::Start, &AtomicBool::new(false)).unwrap();
        let mut all = std::collections::BTreeMap::new();
        loop {
            for block in &page.blocks {
                all.insert(block.index, block.text.clone());
            }
            if !page.later() {
                break;
            }
            page = navigate(&page, Page::Later, &AtomicBool::new(false)).unwrap();
        }
        assert_eq!(all.into_values().collect::<String>(), text);
    }
    #[test]
    fn moving_between_windows_preserves_the_visible_row_anchor() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path(),
            (0..10000)
                .map(|i| format!("line-{i:05}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let page = fixture.read();
        let rows = page.rows(80, true);
        let anchor = rows[0].key;
        let index = rows[0]
            .text
            .strip_prefix("line-")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let earlier = navigate(&page, Page::Earlier, &AtomicBool::new(false)).unwrap();
        let top = earlier.focus(Focus::Move(anchor, -1), 80, 14, true);
        let earlier_rows = earlier.rows(80, true);
        assert_eq!(earlier_rows[top].text, format!("line-{:05}", index - 1));
        assert_eq!(earlier_rows[top + 1].key, anchor);
        let last = earlier_rows.len() - 14;
        let anchor = earlier_rows[last].key;
        let index = earlier_rows[last]
            .text
            .strip_prefix("line-")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let later = navigate(&earlier, Page::Later, &AtomicBool::new(false)).unwrap();
        let top = later.focus(Focus::Move(anchor, 1), 80, 14, true);
        assert_eq!(
            later.rows(80, true)[top].text,
            format!("line-{:05}", index + 1)
        );
    }
    #[test]
    fn rejects_special_files_and_honors_cancellation() {
        let fixture = Fixture::new();
        let path = std::ffi::CString::new(fixture.path().as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let start = Instant::now();
        assert!(
            matches!(open(&fixture.path(), &AtomicBool::new(false)), Err(error) if error.contains("regular log file"))
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(
            matches!(open(&fixture.path(), &AtomicBool::new(true)), Err(error) if error.contains("cancelled"))
        );
    }
    #[test]
    fn horizontal_scrolling_keeps_long_lines_complete_across_cache_windows() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path(),
            (0..10000).map(|i| format!("{i:07}")).collect::<String>(),
        )
        .unwrap();
        let page = fixture.read();
        let rows = page.rows(80, false);
        assert_eq!(rows.len(), 1);
        assert!(page.rows(80, true).len() > 1);
        let anchor = rows[0].at_column(0);
        let earlier = navigate(&page, Page::Earlier, &AtomicBool::new(false)).unwrap();
        assert!(earlier.continues_before() && earlier.continues_after());
        let left = earlier.horizontal(Focus::Horizontal(anchor, -8), 80, false);
        assert_eq!(left, BLOCK_BYTES as usize - 8);
        let rows = earlier.rows(80, false);
        let column = rows[0].text.len() - 80;
        let anchor = rows[0].at_column(column);
        let later = navigate(&earlier, Page::Later, &AtomicBool::new(false)).unwrap();
        let right = later.horizontal(Focus::Horizontal(anchor, 8), 80, false);
        assert_eq!(
            earlier.blocks[0].start + column as u64 + 8,
            later.blocks[0].start + right as u64
        );
    }
    #[test]
    fn line_positions_count_records_including_blank_and_unterminated_lines() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(false);
        for (text, total) in [
            ("", 0),
            ("\n", 1),
            ("\n\n", 2),
            ("one", 1),
            ("one\n", 1),
            ("one\nlast", 2),
            ("\x1b[31mone\x1b[0m\r\n\nlast\r", 3),
            ("one\n\x1b[0m", 1),
        ] {
            fs::write(fixture.path(), text).unwrap();
            let page = fixture.read();
            assert!(page.needs_lines());
            let page = count_lines(&page, &cancel).unwrap();
            let rows = page.rows(80, false);
            assert_eq!(page.line_position(&rows, 0), Some((1.min(total), total)));
            for top in 0..rows.len() {
                assert_eq!(
                    page.line_position(&rows, top),
                    Some((top as u64 + 1, total))
                );
            }
        }
        fs::write(fixture.path(), "abcdefghijkl\nlast").unwrap();
        let page = count_lines(&fixture.read(), &cancel).unwrap();
        let rows = page.rows(4, true);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["abcd", "efgh", "ijkl", "last"]
        );
        for (top, line) in [1, 1, 1, 2].into_iter().enumerate() {
            assert_eq!(page.line_position(&rows, top), Some((line, 2)));
        }
    }
    #[test]
    fn line_counts_survive_navigation_and_ignore_stale_windows_and_reloaded_files() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path(),
            (0..20000)
                .map(|i| format!("line-{i:05}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let tail = fixture.read();
        let mut head = navigate(&tail, Page::Start, &cancel).unwrap();
        let counted = count_lines(&tail, &cancel).unwrap();
        head.merge_lines(&counted);
        assert!(head.needs_lines());
        head.merge_lines(&count_lines(&head, &cancel).unwrap());
        assert_eq!(
            head.line_position(&head.rows(80, false), 0),
            Some((1, 20000))
        );
        for page in [Page::End, Page::At(7), Page::Earlier, Page::Later] {
            let snapshot = navigate(&head, page, &cancel).unwrap();
            assert!(!snapshot.needs_lines());
            let rows = snapshot.rows(80, false);
            for (top, row) in rows.iter().enumerate() {
                let number = row
                    .text
                    .strip_prefix("line-")
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
                    + 1;
                assert_eq!(snapshot.line_position(&rows, top), Some((number, 20000)));
            }
        }
        fs::write(fixture.path(), "replacement\n").unwrap();
        let mut reloaded = fixture.read();
        reloaded.merge_lines(&counted);
        assert!(reloaded.needs_lines());
        let reloaded = count_lines(&reloaded, &cancel).unwrap();
        assert_eq!(
            reloaded.line_position(&reloaded.rows(80, false), 0),
            Some((1, 1))
        );
    }
    #[test]
    fn ansi_sequences_cross_blocks_without_leaking_into_display_search_or_line_counts() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(false);
        for (control, visible) in [
            ("\x1b[31mRED\x1b[0m".to_owned(), "RED"),
            (
                format!("\x1b]0;SECRET\n{}\x1b\\", "payload".repeat(50000)),
                "",
            ),
        ] {
            let prefix = "x".repeat(BLOCK_BYTES as usize - 1);
            fs::write(fixture.path(), format!("{prefix}{control}NEEDLE\nTAIL\n")).unwrap();
            let page = fixture.read();
            let mut normalized = String::new();
            for index in 0..page.source.count() {
                normalized.push_str(
                    &load_block(&page, index, &cancel, Instant::now() + process::TIMEOUT)
                        .unwrap()
                        .text,
                );
            }
            assert_eq!(normalized, format!("{prefix}{visible}NEEDLE\nTAIL\n"));
            let (_, anchor, _) =
                found(search(&page, "NEEDLE", (0, 0), false, false, false, &cancel).unwrap());
            let result = navigate(&page, Page::At(anchor.0), &cancel).unwrap();
            assert!(
                result
                    .marked_rows(80, false, Some("NEEDLE"))
                    .iter()
                    .any(|row| row.text.contains("NEEDLE") && !row.marks.is_empty())
            );
            assert!(matches!(
                search(&page, "SECRET", (0, 0), false, false, false, &cancel).unwrap(),
                SearchResult::NotFound
            ));
            let counted = count_lines(&page, &cancel).unwrap();
            assert_eq!(counted.source.lines.get().unwrap().total, 2);
        }
    }
    #[test]
    fn frequent_matches_only_mark_visible_columns_and_rows() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path(),
            (0..120)
                .map(|i| format!("record-{i:03} {}\n", "x".repeat(45900)))
                .collect::<String>(),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let page = resize(&fixture.read(), 20, &cancel).unwrap();
        for horizontal in [0, 30000] {
            let rows = page.visible_rows(80, false, Some("x"), 0, 20, horizontal);
            assert!(rows.len() > 20);
            for (index, row) in rows.iter().enumerate() {
                if index < 20 {
                    assert_eq!(row.marks.len(), 1);
                    assert!(row.marks[0].1 - row.marks[0].0 <= 80);
                } else {
                    assert!(row.marks.is_empty());
                }
            }
        }
    }
    #[test]
    fn long_records_fill_the_viewport_and_preserve_page_steps_and_line_positions() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path(),
            (0..120)
                .map(|i| format!("record-{i:03} {} END-{i:03}\n", "x".repeat(45900)))
                .collect::<String>(),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let original = fixture.read();
        assert!(original.rows(80, false).len() <= 2);
        let page = resize(&original, 20, &cancel).unwrap();
        let rows = page.rows(80, false);
        assert!(rows.len() >= 20);
        let top = page.focus(Focus::End, 80, 20, false);
        assert!(rows[top].text.starts_with("record-100 "));
        assert!(rows.last().unwrap().text.ends_with("END-119"));
        let page = count_lines(&page, &cancel).unwrap();
        assert_eq!(page.line_position(&rows, top), Some((101, 120)));
        let anchor = rows[top].key;
        let earlier = move_page(&page, Page::Earlier, Focus::Move(anchor, -20), &cancel).unwrap();
        let top = earlier.focus(Focus::Move(anchor, -20), 80, 20, false);
        let rows = earlier.rows(80, false);
        assert!(rows[top].text.starts_with("record-080 "));
        assert_eq!(earlier.line_position(&rows, top), Some((81, 120)));
        let anchor = rows[top].key;
        let later = move_page(&earlier, Page::Later, Focus::Move(anchor, 20), &cancel).unwrap();
        let top = later.focus(Focus::Move(anchor, 20), 80, 20, false);
        assert!(later.rows(80, false)[top].text.starts_with("record-100 "));
        let larger = resize(&page, 40, &cancel).unwrap();
        let top = larger.focus(Focus::End, 80, 40, false);
        let rows = larger.rows(80, false);
        assert!(rows[top].text.starts_with("record-080 "));
        assert!(larger.blocks.len() <= MAX_CACHE_BLOCKS);
    }
    #[test]
    fn line_index_metadata_stays_bounded_and_counting_is_cancellable() {
        let fixture = Fixture::new();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(fixture.path())
            .unwrap();
        file.set_len((LINE_CHECKPOINTS + 2) * BLOCK_BYTES).unwrap();
        let page = fixture.read();
        assert!(
            matches!(count_lines(&page, &AtomicBool::new(true)), Err(error) if error.contains("cancelled"))
        );
        assert!(page.source.lines.get().is_none());
        let page = count_lines(&page, &AtomicBool::new(false)).unwrap();
        let index = page.source.lines.get().unwrap();
        assert!(index.checkpoints.len() <= LINE_CHECKPOINTS as usize);
        assert_eq!(page.line_position(&page.rows(80, false), 0), Some((1, 1)));
    }
    #[test]
    fn sparse_terabyte_tail_uses_bounded_hole_queries() {
        use std::os::fd::AsRawFd;
        let fixture = Fixture::new();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(fixture.path())
            .unwrap();
        file.set_len(1u64 << 40).unwrap();
        let probe = unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_DATA) };
        if probe >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ENXIO) {
            return;
        }
        let page = fixture.read();
        let parser = page.source.parser(&AtomicBool::new(false)).unwrap();
        assert!(parser.hole_queries <= LINE_CHECKPOINTS as usize + 16);
        drop(parser);
        let counted = count_lines(&page, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            counted.line_position(&counted.rows(80, false), 0),
            Some((1, 1))
        );
        assert!(
            page.source
                .hole_queries
                .load(std::sync::atomic::Ordering::Relaxed)
                <= LINE_CHECKPOINTS as usize + 16
        );
        assert!(
            page.blocks
                .iter()
                .map(|block| block.text.len())
                .sum::<usize>()
                < 128 * 1024
        );
    }
    #[test]
    fn huge_sparse_files_load_without_reading_the_entire_file() {
        let fixture = Fixture::new();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(fixture.path())
            .unwrap();
        let size = 3 * 1024 * 1024 * 1024u64;
        file.set_len(size).unwrap();
        file.write_all_at(b"HEAD\n", 0).unwrap();
        file.write_all_at(b"\nTAIL\n", size - 6).unwrap();
        let page = fixture.read();
        assert_eq!(page.source.size, size);
        assert!(
            page.blocks
                .iter()
                .map(|block| block.text.len())
                .sum::<usize>()
                < 128 * 1024
        );
        assert_eq!(page.rows(80, true).last().unwrap().text, "TAIL");
        let page = navigate(&page, Page::Start, &AtomicBool::new(false)).unwrap();
        assert_eq!(page.rows(80, true)[0].text, "HEAD");
    }
    fn found(result: SearchResult) -> (Snapshot, Anchor, bool) {
        match result {
            SearchResult::Found {
                snapshot,
                anchor,
                wrapped,
            } => (snapshot, anchor, wrapped),
            SearchResult::NotFound => panic!("expected a match"),
        }
    }
    #[test]
    fn search_moves_in_both_directions_and_wraps_with_unicode_literal_text() {
        let fixture = Fixture::new();
        fs::write(
            fixture.path(),
            "head\n训练[0] first\nplain\n训练[0] second\ntail\n",
        )
        .unwrap();
        let page = fixture.read();
        let cancel = AtomicBool::new(false);
        let (page, first, wrapped) =
            found(search(&page, "训练[0]", (0, 0), false, false, true, &cancel).unwrap());
        assert!(!wrapped);
        let (page, second, wrapped) =
            found(search(&page, "训练[0]", first, false, true, true, &cancel).unwrap());
        assert!(!wrapped && second > first);
        let (page, previous, wrapped) =
            found(search(&page, "训练[0]", second, true, true, true, &cancel).unwrap());
        assert!(!wrapped && previous == first);
        let (page, previous, wrapped) =
            found(search(&page, "训练[0]", first, true, true, true, &cancel).unwrap());
        assert!(wrapped && previous == second);
        let (_, next, wrapped) =
            found(search(&page, "训练[0]", second, false, true, true, &cancel).unwrap());
        assert!(wrapped && next == first);
        assert!(matches!(
            search(&page, "missing", first, false, false, true, &cancel).unwrap(),
            SearchResult::NotFound
        ));
        assert!(
            matches!(search(&page, "训练[0]", first, false, false, true, &AtomicBool::new(true)), Err(error) if error.contains("cancelled"))
        );
    }
    #[test]
    fn search_finds_matches_crossing_file_windows() {
        let fixture = Fixture::new();
        let mut text = vec![b'x'; 7 * BLOCK_BYTES as usize + 100];
        let start = 2 * BLOCK_BYTES as usize - 2;
        text[start..start + 6].copy_from_slice(b"NEEDLE");
        fs::write(fixture.path(), text).unwrap();
        let page = fixture.read();
        let cancel = AtomicBool::new(false);
        let (page, anchor, _) =
            found(search(&page, "NEEDLE", (0, 0), false, false, false, &cancel).unwrap());
        assert_eq!(anchor, (1, BLOCK_BYTES as usize - 2));
        let rows = page.marked_rows(80, false, Some("NEEDLE"));
        assert!(
            rows.iter()
                .any(|row| row.contains(anchor) && !row.marks.is_empty())
        );
        let (_, reverse, _) =
            found(search(&page, "NEEDLE", (7, 99), true, false, false, &cancel).unwrap());
        assert_eq!(reverse, anchor);
    }
    #[test]
    fn repeated_search_preserves_overlapping_matches() {
        let fixture = Fixture::new();
        fs::write(fixture.path(), "bananana").unwrap();
        let page = fixture.read();
        let cancel = AtomicBool::new(false);
        let (_, first, _) =
            found(search(&page, "ana", (0, 0), false, false, false, &cancel).unwrap());
        let (_, second, _) =
            found(search(&page, "ana", first, false, true, false, &cancel).unwrap());
        let (_, third, _) =
            found(search(&page, "ana", second, false, true, false, &cancel).unwrap());
        assert_eq!([first, second, third], [(0, 1), (0, 3), (0, 5)]);
        let (_, previous, _) =
            found(search(&page, "ana", second, true, true, false, &cancel).unwrap());
        assert_eq!(previous, first);
        assert_eq!(
            page.marked_rows(80, false, Some("ana"))[0].marks,
            [(1, 4), (3, 6), (5, 8)]
        );
    }
    #[test]
    fn search_highlighting_survives_wrapping_wide_text() {
        let fixture = Fixture::new();
        fs::write(fixture.path(), "aaaaaa训练needlezzzz").unwrap();
        let rows = fixture.read().marked_rows(8, true, Some("训练needle"));
        let pieces = rows
            .iter()
            .flat_map(|row| {
                row.marks
                    .iter()
                    .map(|&(start, end)| row.text[start..end].to_owned())
            })
            .collect::<String>();
        assert_eq!(pieces, "训练needle");
        assert_eq!(rows.iter().filter(|row| !row.marks.is_empty()).count(), 2);
    }
}
