use crate::fields::Row;
use std::time::{Duration, Instant};

pub fn identity(row: &Row) -> (String, String) {
    let id = row
        .get("stepid")
        .filter(|s| !s.is_empty())
        .or_else(|| row.get("jobid"));
    (
        row.get("cluster").cloned().unwrap_or_default(),
        id.cloned().unwrap_or_default(),
    )
}

#[derive(Default)]
pub struct Selection {
    pub selected: usize,
    pub top: usize,
    pub horizontal: usize,
}
impl Selection {
    pub fn replace(&mut self, old: &[Row], new: &[Row]) {
        let key = old.get(self.selected).map(identity);
        self.selected = key
            .and_then(|key| new.iter().position(|row| identity(row) == key))
            .unwrap_or(self.selected.min(new.len().saturating_sub(1)));
        if new.is_empty() {
            self.top = 0;
        }
    }
    pub fn visible(&mut self, total: usize, capacity: usize) {
        let capacity = capacity.max(1);
        self.selected = self.selected.min(total.saturating_sub(1));
        self.top = self.top.min(total.saturating_sub(capacity));
        if self.selected < self.top {
            self.top = self.selected;
        }
        if self.selected >= self.top.saturating_add(capacity) {
            self.top = self.selected.saturating_add(1).saturating_sub(capacity);
        }
    }
    pub fn move_by(&mut self, delta: isize, total: usize, capacity: usize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(total.saturating_sub(1));
        self.visible(total, capacity);
    }
}

pub struct Schedule {
    pub interval: Duration,
    pub paused: bool,
    pending: bool,
    next: Instant,
}
impl Schedule {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            paused: false,
            pending: true,
            next: Instant::now(),
        }
    }
    pub fn due(&self, now: Instant, busy: bool) -> bool {
        !busy && (self.pending || (!self.paused && now >= self.next))
    }
    pub fn started(&mut self) {
        self.pending = false;
    }
    pub fn completed(&mut self, now: Instant) {
        self.next = now.checked_add(self.interval).unwrap_or(now);
    }
    pub fn refresh(&mut self) {
        self.pending = true;
    }
    pub fn toggle(&mut self) {
        self.paused = !self.paused;
        if !self.paused {
            self.refresh();
        }
    }
    pub fn adjust(&mut self, faster: bool) {
        let seconds = self.interval.as_secs();
        let seconds = if faster {
            seconds.saturating_sub(1).max(1)
        } else {
            seconds.saturating_add(1)
        };
        let duration = Duration::from_secs(seconds);
        if let Some(next) = Instant::now().checked_add(duration) {
            self.interval = duration;
            self.next = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(cluster: &str, id: &str) -> Row {
        Row::from([
            ("cluster".into(), cluster.into()),
            ("jobid".into(), id.into()),
        ])
    }
    #[test]
    fn selection_follows_identity_across_reorder_deletion_and_empty_results() {
        let a = row("alpha", "42");
        let b = row("beta", "42");
        let c = row("beta", "43");
        let mut state = Selection {
            selected: 1,
            ..Selection::default()
        };
        state.replace(&[a.clone(), b.clone()], &[b.clone(), a.clone()]);
        assert_eq!(state.selected, 0);
        state.replace(&[b.clone(), a.clone()], &[a.clone(), c.clone()]);
        assert_eq!(state.selected, 0);
        state.move_by(100, 2, 1);
        assert_eq!(state.selected, 1);
        assert_eq!(state.top, 1);
        state.visible(2, 10);
        assert_eq!(state.top, 0);
        state.replace(&[a, c], &[]);
        state.visible(0, 0);
        assert_eq!(state.selected, 0);
        assert_eq!(state.top, 0);
        assert_ne!(identity(&row("alpha", "42")), identity(&row("beta", "42")));
    }
    #[test]
    fn refreshes_are_coalesced_pauseable_and_frequency_changes_are_bounded() {
        let now = Instant::now();
        let mut schedule = Schedule::new(Duration::from_secs(5));
        assert!(schedule.due(now, false));
        schedule.started();
        schedule.completed(now);
        assert!(!schedule.due(now, false));
        schedule.refresh();
        schedule.refresh();
        assert!(!schedule.due(now, true));
        assert!(schedule.due(now, false));
        schedule.started();
        schedule.toggle();
        assert!(!schedule.due(now + Duration::from_secs(60), false));
        schedule.refresh();
        assert!(schedule.due(now, false));
        schedule.started();
        schedule.toggle();
        assert!(schedule.due(now, false));
        for _ in 0..10 {
            schedule.adjust(true);
        }
        assert_eq!(schedule.interval.as_secs(), 1);
        schedule.adjust(false);
        assert_eq!(schedule.interval.as_secs(), 2);
    }
}
