//! When does a mouse move take input to another screen?
//!
//! Only a push outwards against an edge that leads somewhere: the move would
//! put the cursor beyond the edge, or, where Windows already stopped the
//! cursor at the edge, leaves it where it is several times in a row. Moving
//! along an edge or away from it never takes input, so a cursor next to or
//! on an edge can always move freely back into the screen.

use kvmbridge_core::layout::Desktop;
use kvmbridge_core::types::Edge;

/// Moves in a row that leave a cursor on an edge where it was.
pub const STILL_PUSHES: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Check {
    /// Take input at this point on this edge.
    pub take: Option<(Edge, (i32, i32))>,
    /// The move left a cursor on an edge where it was (a push Windows
    /// stopped, or a move too small to show).
    pub still: bool,
}

/// Checks a move of the cursor from `old` to `new` (as a low-level mouse
/// hook reports it, before the cursor moves) against `edges`.
/// `still_before` counts the moves just before that were `still`, and
/// `cooling` says whether an edge is to be left alone for now.
pub fn check(
    desk: &Desktop,
    edges: &[Edge],
    cooling: impl Fn(Edge) -> bool,
    old: (i32, i32),
    new: (i32, i32),
    still_before: u32,
) -> Check {
    let mut still = false;
    for &edge in edges {
        if cooling(edge) {
            continue;
        }
        for (fixed, a, z) in desk.outer_segments(edge) {
            let (fixed, a, z) = (fixed.round() as i32, a.round() as i32, z.round() as i32);
            // The pixel row/column on the edge, the positions across and
            // along it, and which way is out.
            let (line, across_old, across_new, along_old, along_new, out) = match edge {
                Edge::Left => (fixed, old.0, new.0, old.1, new.1, -1),
                Edge::Right => (fixed - 1, old.0, new.0, old.1, new.1, 1),
                Edge::Top => (fixed, old.1, new.1, old.0, new.0, -1),
                Edge::Bottom => (fixed - 1, old.1, new.1, old.0, new.0, 1),
            };
            if along_old < a || along_old >= z || (across_old - line) * out > 0 {
                continue;
            }
            let beyond = (across_new - line) * out > 0;
            let here_still = across_old == line && new == old;
            still |= here_still;
            if beyond || (here_still && still_before + 1 >= STILL_PUSHES) {
                let along = along_new.clamp(a, z - 1);
                let at = match edge {
                    Edge::Left | Edge::Right => (line, along),
                    Edge::Top | Edge::Bottom => (along, line),
                };
                return Check { take: Some((edge, at)), still: here_still };
            }
        }
    }
    Check { take: None, still }
}
