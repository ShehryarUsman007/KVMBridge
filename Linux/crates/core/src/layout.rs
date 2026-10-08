//! Screen arrangement: which device sits next to which on a grid, plus the
//! geometry helpers used to move the (virtual) cursor between desktops.

use crate::types::{DeviceId, Edge, Point, Rect};
use serde::{Deserialize, Serialize};

/// The monitors of one device, in that device's global logical coordinates.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Desktop {
    pub monitors: Vec<Rect>,
}

impl Desktop {
    pub fn new(monitors: Vec<Rect>) -> Self {
        let monitors = monitors.into_iter().filter(|m| !m.is_empty()).collect();
        Desktop { monitors }
    }

    pub fn is_empty(&self) -> bool {
        self.monitors.is_empty()
    }

    /// Bounding box of all monitors.
    pub fn bounds(&self) -> Rect {
        let mut it = self.monitors.iter();
        match it.next() {
            None => Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Some(first) => it.fold(*first, |acc, r| acc.union(r)),
        }
    }

    pub fn contains(&self, p: Point) -> bool {
        self.monitors.iter().any(|m| m.contains(p))
    }

    /// Nearest point that lies on one of the monitors.
    pub fn clamp(&self, p: Point) -> Point {
        if self.monitors.is_empty() {
            return self.bounds().clamp(p);
        }
        if self.contains(p) {
            return p;
        }
        let mut best = p;
        let mut best_d = f64::INFINITY;
        for m in &self.monitors {
            let c = m.clamp(p);
            let d = (c.x - p.x).powi(2) + (c.y - p.y).powi(2);
            if d < best_d {
                best_d = d;
                best = c;
            }
        }
        best
    }

    /// Centre of the first (primary) monitor.
    pub fn center(&self) -> Point {
        let r = self.monitors.first().copied().unwrap_or_else(|| self.bounds());
        Point::new(r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    /// Outer edge segments of the desktop on the given side, as
    /// `(fixed coordinate, start, end)` where `fixed` is x for left/right edges
    /// and y for top/bottom edges. Only monitor edges that lie on the bounding
    /// box are considered "outer" — these are where barriers are placed.
    pub fn outer_segments(&self, edge: Edge) -> Vec<(f64, f64, f64)> {
        let b = self.bounds();
        let eps = 0.5;
        self.monitors
            .iter()
            .filter_map(|m| match edge {
                Edge::Left if (m.x - b.x).abs() < eps => Some((m.x, m.y, m.bottom())),
                Edge::Right if (m.right() - b.right()).abs() < eps => Some((m.right(), m.y, m.bottom())),
                Edge::Top if (m.y - b.y).abs() < eps => Some((m.y, m.x, m.right())),
                Edge::Bottom if (m.bottom() - b.bottom()).abs() < eps => Some((m.bottom(), m.x, m.right())),
                _ => None,
            })
            .collect()
    }

    /// Relative position (0..1) of `p` along the given edge of the bounding box.
    pub fn fraction_along(&self, edge: Edge, p: Point) -> f64 {
        let b = self.bounds();
        let t = if edge.is_horizontal_neighbor() {
            (p.y - b.y) / b.h.max(1.0)
        } else {
            (p.x - b.x) / b.w.max(1.0)
        };
        t.clamp(0.0, 1.0)
    }

    /// Point just inside this desktop on `edge`, at fraction `t` along it.
    pub fn entry_point(&self, edge: Edge, t: f64, inset: f64) -> Point {
        let b = self.bounds();
        let t = t.clamp(0.0, 1.0);
        let p = match edge {
            Edge::Left => Point::new(b.x + inset, b.y + t * (b.h - 1.0)),
            Edge::Right => Point::new(b.right() - 1.0 - inset, b.y + t * (b.h - 1.0)),
            Edge::Top => Point::new(b.x + t * (b.w - 1.0), b.y + inset),
            Edge::Bottom => Point::new(b.x + t * (b.w - 1.0), b.bottom() - 1.0 - inset),
        };
        // The point may fall into a gap between differently sized monitors:
        // pull it onto the closest monitor, preferring monitors touching that edge.
        if self.contains(p) {
            return p;
        }
        let segs = self.outer_segments(edge);
        let mut best = self.clamp(p);
        let mut best_d = f64::INFINITY;
        for (fixed, a, z) in segs {
            let c = if edge.is_horizontal_neighbor() {
                let x = if edge == Edge::Left { fixed + inset } else { fixed - 1.0 - inset };
                Point::new(x, p.y.clamp(a, z - 1.0))
            } else {
                let y = if edge == Edge::Top { fixed + inset } else { fixed - 1.0 - inset };
                Point::new(p.x.clamp(a, z - 1.0), y)
            };
            let d = (c.x - p.x).powi(2) + (c.y - p.y).powi(2);
            if d < best_d {
                best_d = d;
                best = c;
            }
        }
        best
    }
}

/// Where the cursor of a device leaving on `edge` at `p` should appear on the
/// neighbouring desktop (which it enters through the opposite edge).
pub fn map_across(from: &Desktop, p: Point, edge: Edge, to: &Desktop) -> Point {
    let t = from.fraction_along(edge, p);
    to.entry_point(edge.opposite(), t, 1.0)
}

/// Result of moving a virtual cursor on a remote desktop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// The cursor stays on this desktop at the given (clamped) position.
    Inside(Point),
    /// The cursor pushed through an outer edge. `at` is the clamped position
    /// on the edge (used to compute where it enters the neighbour).
    Exit { edge: Edge, at: Point },
}

/// Apply a relative motion to a virtual cursor on `desk`.
pub fn step(desk: &Desktop, cur: Point, dx: f64, dy: f64) -> Step {
    let next = Point::new(cur.x + dx, cur.y + dy);
    if desk.contains(next) {
        return Step::Inside(next);
    }
    let b = desk.bounds();
    // How far past each side of the bounding box did we go?
    let over_left = b.x - next.x;
    let over_right = next.x - (b.right() - 1.0);
    let over_top = b.y - next.y;
    let over_bottom = next.y - (b.bottom() - 1.0);
    let candidates = [
        (Edge::Left, over_left, dx < 0.0),
        (Edge::Right, over_right, dx > 0.0),
        (Edge::Top, over_top, dy < 0.0),
        (Edge::Bottom, over_bottom, dy > 0.0),
    ];
    let mut exit: Option<(Edge, f64)> = None;
    for (edge, over, moving) in candidates {
        if over > 0.0 && moving && exit.is_none_or(|(_, o)| over > o) {
            exit = Some((edge, over));
        }
    }
    let clamped = desk.clamp(next);
    match exit {
        Some((edge, _)) => {
            // Only count it as leaving if the cursor was actually on that outer
            // edge (it was on a monitor that touches the bounding box side).
            let on_outer = desk.outer_segments(edge).iter().any(|&(_, a, z)| {
                let along = if edge.is_horizontal_neighbor() { clamped.y } else { clamped.x };
                along >= a - 0.5 && along < z + 0.5
            });
            if on_outer {
                Step::Exit { edge, at: clamped }
            } else {
                Step::Inside(clamped)
            }
        }
        None => Step::Inside(clamped),
    }
}

/// A device placed on the arrangement grid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub device: DeviceId,
    pub col: i32,
    pub row: i32,
    /// User facing "Screen N" number.
    pub number: u32,
    /// Display name cached so that devices not currently reachable still show a label.
    #[serde(default)]
    pub name: String,
}

/// The shared arrangement of screens. Newest `(rev, author)` wins when two
/// devices disagree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub rev: u64,
    pub author: DeviceId,
    pub slots: Vec<Slot>,
    /// The computer whose keyboard and mouse control all screens (`None`:
    /// any of them). While it does, the keyboards and mice of the other
    /// computers are paused so they cannot interfere.
    #[serde(default)]
    pub controller: Option<DeviceId>,
}

impl Layout {
    pub fn single(me: DeviceId, name: &str) -> Self {
        Layout {
            rev: 0,
            author: me,
            slots: vec![Slot { device: me, col: 0, row: 0, number: 1, name: name.to_string() }],
            controller: None,
        }
    }

    pub fn version(&self) -> (u64, DeviceId) {
        (self.rev, self.author)
    }

    pub fn is_newer_than(&self, other: &Layout) -> bool {
        self.version() > other.version()
    }

    pub fn slot(&self, dev: DeviceId) -> Option<&Slot> {
        self.slots.iter().find(|s| s.device == dev)
    }

    pub fn contains(&self, dev: DeviceId) -> bool {
        self.slot(dev).is_some()
    }

    pub fn at(&self, col: i32, row: i32) -> Option<&Slot> {
        self.slots.iter().find(|s| s.col == col && s.row == row)
    }

    pub fn number_of(&self, dev: DeviceId) -> Option<u32> {
        self.slot(dev).map(|s| s.number)
    }

    /// Device directly adjacent on the given side, if any.
    pub fn neighbor(&self, dev: DeviceId, edge: Edge) -> Option<DeviceId> {
        let s = self.slot(dev)?;
        let (dc, dr) = edge.grid_delta();
        self.at(s.col + dc, s.row + dr).map(|n| n.device)
    }

    pub fn neighbors(&self, dev: DeviceId) -> Vec<(Edge, DeviceId)> {
        Edge::ALL
            .iter()
            .filter_map(|&e| self.neighbor(dev, e).map(|n| (e, n)))
            .collect()
    }

    fn free_number(&self) -> u32 {
        (1..).find(|n| !self.slots.iter().any(|s| s.number == *n)).unwrap()
    }

    /// Nearest free cell to (col,row), searching outward ring by ring and
    /// preferring cells to the right, then below.
    pub fn nearest_free(&self, col: i32, row: i32) -> (i32, i32) {
        if self.at(col, row).is_none() {
            return (col, row);
        }
        for radius in 1i32..64 {
            let mut ring = Vec::new();
            for dr in -radius..=radius {
                for dc in -radius..=radius {
                    if dr.abs().max(dc.abs()) == radius {
                        ring.push((col + dc, row + dr));
                    }
                }
            }
            // Prefer same row to the right, then other rows, then left.
            ring.sort_by_key(|&(c, r)| ((r - row).abs(), if c >= col { 0 } else { 1 }, (c - col).abs()));
            if let Some(&cell) = ring.iter().find(|&&(c, r)| self.at(c, r).is_none()) {
                return cell;
            }
        }
        (col + 64, row)
    }

    /// Add a device next to `anchor` (to its right when possible).
    pub fn place_next_to(&mut self, dev: DeviceId, name: &str, anchor: DeviceId) {
        if self.contains(dev) {
            return;
        }
        let (c, r) = self.slot(anchor).map(|s| (s.col + 1, s.row)).unwrap_or((0, 0));
        let (c, r) = self.nearest_free(c, r);
        let number = self.free_number();
        self.slots.push(Slot { device: dev, col: c, row: r, number, name: name.to_string() });
    }

    pub fn remove(&mut self, dev: DeviceId) -> bool {
        let before = self.slots.len();
        self.slots.retain(|s| s.device != dev);
        if self.controller == Some(dev) {
            self.controller = None;
        }
        before != self.slots.len()
    }

    /// Union of two layouts used when two devices are paired: everything from
    /// the newer layout keeps its place, devices only known to the older one
    /// are added at their old cell (or the nearest free one).
    pub fn merged_with(&self, other: &Layout) -> Layout {
        let (newer, older) = if self.is_newer_than(other) { (self, other) } else { (other, self) };
        let mut out = newer.clone();
        for s in &older.slots {
            if out.contains(s.device) {
                continue;
            }
            let (c, r) = out.nearest_free(s.col, s.row);
            let number = if out.slots.iter().any(|o| o.number == s.number) { out.free_number() } else { s.number };
            out.slots.push(Slot { device: s.device, col: c, row: r, number, name: s.name.clone() });
        }
        out.rev = self.rev.max(other.rev);
        out
    }

    /// Normalises the grid so the minimum column/row is 0 and numbers are unique.
    pub fn normalized(mut self) -> Layout {
        if let (Some(min_c), Some(min_r)) =
            (self.slots.iter().map(|s| s.col).min(), self.slots.iter().map(|s| s.row).min())
        {
            for s in &mut self.slots {
                s.col -= min_c;
                s.row -= min_r;
            }
        }
        // Drop duplicate devices / cells (keep first).
        let mut seen_dev = std::collections::HashSet::new();
        let mut seen_cell = std::collections::HashSet::new();
        self.slots.retain(|s| seen_dev.insert(s.device) && seen_cell.insert((s.col, s.row)));
        if self.controller.is_some_and(|c| !seen_dev.contains(&c)) {
            self.controller = None;
        }
        let mut used = std::collections::HashSet::new();
        let mut needs_number = Vec::new();
        for (i, s) in self.slots.iter().enumerate() {
            if s.number == 0 || !used.insert(s.number) {
                needs_number.push(i);
            }
        }
        for i in needs_number {
            let n = (1..).find(|n| !used.contains(n)).unwrap();
            used.insert(n);
            self.slots[i].number = n;
        }
        self
    }
}
