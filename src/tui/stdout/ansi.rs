use super::{Source, check, process};
use crate::util::Result;
use std::{os::fd::AsRawFd, sync::atomic::AtomicBool, time::Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
}
impl State {
    fn byte(&mut self, byte: u8) -> bool {
        match *self {
            Self::Text => {
                if byte == 0x1b {
                    *self = Self::Escape;
                    false
                } else {
                    true
                }
            }
            Self::Escape => {
                *self = match byte {
                    b'[' => Self::Csi,
                    b']' => Self::Osc,
                    0x1b => Self::Escape,
                    _ => Self::Text,
                };
                !matches!(byte, b'[' | b']' | 0x1b)
            }
            Self::Csi => {
                if (0x40..=0x7e).contains(&byte) {
                    *self = Self::Text;
                    false
                } else if !(0x20..=0x3f).contains(&byte) {
                    *self = Self::Text;
                    true
                } else {
                    false
                }
            }
            Self::Osc => {
                if byte == 7 {
                    *self = Self::Text;
                } else if byte == 0x1b {
                    *self = Self::OscEscape;
                }
                false
            }
            Self::OscEscape => {
                *self = match byte {
                    b'\\' | 7 => Self::Text,
                    0x1b => Self::OscEscape,
                    _ => Self::Osc,
                };
                false
            }
        }
    }
    fn advance(&mut self, bytes: &[u8]) {
        if *self == Self::Text && !bytes.contains(&0x1b)
            || *self == Self::Osc && !bytes.contains(&0x1b) && !bytes.contains(&7)
        {
            return;
        }
        for &byte in bytes {
            self.byte(byte);
        }
    }
    fn zeros(&mut self) {
        *self = match *self {
            Self::Text | Self::Escape | Self::Csi => Self::Text,
            Self::Osc | Self::OscEscape => Self::Osc,
        };
    }
}

pub(super) struct Index {
    stride: u64,
    states: Vec<State>,
    recent: (u64, State),
    #[cfg(test)]
    pub(super) hole_queries: usize,
}
impl Index {
    pub fn new(size: u64) -> Self {
        let stride = size
            .div_ceil(super::LINE_CHECKPOINTS)
            .div_ceil(super::BLOCK_BYTES)
            .max(1)
            * super::BLOCK_BYTES;
        Self {
            stride,
            states: vec![State::Text],
            recent: (0, State::Text),
            #[cfg(test)]
            hole_queries: 0,
        }
    }
    fn remember(&mut self, offset: u64, state: State) {
        if offset == self.states.len() as u64 * self.stride {
            self.states.push(state);
        }
        self.recent = (offset, state);
    }
    pub fn prepare(&mut self, source: &Source, cancel: &AtomicBool) -> Result<()> {
        self.at(source, source.size, cancel).map(|_| ())
    }
    fn at(&mut self, source: &Source, target: u64, cancel: &AtomicBool) -> Result<State> {
        let point = (target / self.stride).min(self.states.len() as u64 - 1);
        let (mut offset, mut state) = (point * self.stride, self.states[point as usize]);
        if self.recent.0 >= offset && self.recent.0 <= target {
            (offset, state) = self.recent;
        }
        while offset < target {
            check(cancel, Instant::now() + process::TIMEOUT)?;
            let next_point = (offset / self.stride + 1) * self.stride;
            let mut end = target.min(next_point).min(offset.saturating_add(64 * 1024));
            // Sparse holes contain zero bytes and cannot introduce an escape.
            #[cfg(test)]
            {
                self.hole_queries += 1;
            }
            let next_data = unsafe {
                libc::lseek(
                    source.file.as_raw_fd(),
                    offset as libc::off_t,
                    libc::SEEK_DATA,
                )
            };
            let hole_end = if next_data >= 0 {
                Some(next_data as u64)
            } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
                Some(source.size)
            } else {
                None
            };
            if let Some(data) = hole_end.filter(|&data| data > offset) {
                end = target.min(next_point).min(data);
                state.zeros();
            } else {
                state.advance(&source.bytes(
                    offset,
                    end,
                    cancel,
                    Instant::now() + process::TIMEOUT,
                )?);
            }
            offset = end;
            self.remember(offset, state);
        }
        Ok(state)
    }
    pub fn text(
        &mut self,
        source: &Source,
        start: u64,
        bytes: &[u8],
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>> {
        let mut state = self.at(source, start, cancel)?;
        let mut text = Vec::with_capacity(bytes.len());
        for (index, &byte) in bytes.iter().enumerate() {
            if state.byte(byte) {
                text.push(byte);
            }
            self.remember(start + index as u64 + 1, state);
        }
        Ok(text)
    }
}
