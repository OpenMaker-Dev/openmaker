//! Layout object translation shared by desktop draft edits and authoring.
//! Callers supply frames for which movement is allowed and apply the plan
//! through their draft or guarded frame writer.

use crate::{layout_placement::Refusal, GeomRect};

pub const ID: &str = "move_objects";
pub const LABEL: &str = "Move layout objects";

/// Requested or effective translation in layout points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Delta {
    pub dx: f64,
    pub dy: f64,
}

/// The distance requested by one Layout mode arrow-key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NudgeStep {
    OnePoint,
    TenPoints,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NudgeDirection {
    Left,
    Right,
    Up,
    Down,
}

impl NudgeDirection {
    /// Translate a direction and step into a delta for the shared move planner.
    pub fn delta(self, step: NudgeStep) -> Delta {
        let points = match step {
            NudgeStep::OnePoint => 1.0,
            NudgeStep::TenPoints => 10.0,
        };
        let (dx, dy) = match self {
            Self::Left => (-points, 0.0),
            Self::Right => (points, 0.0),
            Self::Up => (0.0, -points),
            Self::Down => (0.0, points),
        };
        Delta { dx, dy }
    }
}

#[derive(Debug, PartialEq)]
pub struct MovePlan {
    /// The rounded translation, clamped for the entire selection.
    pub delta: Delta,
    /// The resulting frames in the same order as the input.
    pub frames: Vec<GeomRect>,
}

/// Plan a move for a nonempty selection with finite coordinates. Whole
/// points are required for origins in the object index. The selection stops
/// at the left and top edges and each frame keeps its original size. The
/// guarded frame writer checks part and layout bounds when saving.
pub fn plan(
    frames: impl IntoIterator<Item = GeomRect>,
    requested: Delta,
) -> Result<MovePlan, Refusal> {
    let mut frames: Vec<_> = frames.into_iter().collect();
    if frames.is_empty() {
        return Err(Refusal::Invalid(
            "Select at least one object to move.".into(),
        ));
    }
    if !requested.dx.is_finite() || !requested.dy.is_finite() {
        return Err(Refusal::Invalid("The move needs a finite offset.".into()));
    }
    let mut min_left = f64::INFINITY;
    let mut min_top = f64::INFINITY;
    for frame in &frames {
        if !finite(*frame) {
            return Err(Refusal::Invalid(
                "Every object needs a finite frame.".into(),
            ));
        }
        min_left = min_left.min(frame.left.round());
        min_top = min_top.min(frame.top.round());
    }
    let delta = Delta {
        dx: requested.dx.round().max(-min_left),
        dy: requested.dy.round().max(-min_top),
    };
    for frame in &mut frames {
        let left = (frame.left + delta.dx).round().max(0.0);
        let top = (frame.top + delta.dy).round().max(0.0);
        *frame = GeomRect {
            left,
            top,
            right: left + frame.width(),
            bottom: top + frame.height(),
        };
        if !finite(*frame) {
            return Err(Refusal::Invalid(
                "The move produces a frame outside the coordinate range.".into(),
            ));
        }
    }
    Ok(MovePlan { delta, frames })
}

fn finite(frame: GeomRect) -> bool {
    [frame.left, frame.top, frame.right, frame.bottom]
        .into_iter()
        .all(f64::is_finite)
}
