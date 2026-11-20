//! Layout mode's Arrange > Align, Distribute and Resize To (client-ui.md,
//! "Layout mode: Align, Distribute and Resize To"): new frames for objects
//! relative to one another. The frames are then written as any moved
//! objects' are (`layout_objects::plan`, `DataProvider::set_layout_object_
//! frames`, `om-author`'s `set_object_frames`), so the host's checks, the
//! revision conflict and the read-back are the same.
//!
//! The rules are Claris's documented ones where the help gives them, and
//! OpenMaker's otherwise (marked "inferred"): left and top edges land on
//! whole points, as the object index holds them.

use om_layout::Rect;

use crate::layout_objects::ObjectFrame;
use crate::layout_placement::Refusal;

/// One of the Arrange menu's Align, Distribute and Resize To commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arrangement {
    /// Left edges to the leftmost left edge.
    AlignLeftEdges,
    /// Centers on one vertical line, at the middle of the objects' extent
    /// (inferred). With left edges on whole points, applying it again can
    /// move an object by one more point.
    AlignVerticalCenters,
    /// Right edges to the rightmost right edge.
    AlignRightEdges,
    /// Top edges to the topmost top edge.
    AlignTopEdges,
    /// Centers on one horizontal line, at the middle of the objects'
    /// extent (inferred), as [`Arrangement::AlignVerticalCenters`].
    AlignHorizontalCenters,
    /// Bottom edges to the bottommost bottom edge.
    AlignBottomEdges,
    /// The leftmost and the rightmost objects stay; the others move so the
    /// gaps between neighbours are equal (inferred). Objects wider together
    /// than the space between the outermost edges get evenly spaced left
    /// edges instead, so their order stays (OpenMaker's choice).
    DistributeHorizontally,
    /// As [`Arrangement::DistributeHorizontally`], along top edges.
    DistributeVertically,
    SmallestWidth,
    SmallestHeight,
    SmallestWidthAndHeight,
    LargestWidth,
    LargestHeight,
    LargestWidthAndHeight,
}

impl Arrangement {
    /// Every arrangement, in menu order.
    pub const ALL: [Arrangement; 14] = [
        Arrangement::AlignLeftEdges,
        Arrangement::AlignVerticalCenters,
        Arrangement::AlignRightEdges,
        Arrangement::AlignTopEdges,
        Arrangement::AlignHorizontalCenters,
        Arrangement::AlignBottomEdges,
        Arrangement::DistributeHorizontally,
        Arrangement::DistributeVertically,
        Arrangement::SmallestWidth,
        Arrangement::SmallestHeight,
        Arrangement::SmallestWidthAndHeight,
        Arrangement::LargestWidth,
        Arrangement::LargestHeight,
        Arrangement::LargestWidthAndHeight,
    ];

    /// The stable id: `om-author`'s `arrangement`.
    pub fn id(self) -> &'static str {
        match self {
            Arrangement::AlignLeftEdges => "align_left_edges",
            Arrangement::AlignVerticalCenters => "align_vertical_centers",
            Arrangement::AlignRightEdges => "align_right_edges",
            Arrangement::AlignTopEdges => "align_top_edges",
            Arrangement::AlignHorizontalCenters => "align_horizontal_centers",
            Arrangement::AlignBottomEdges => "align_bottom_edges",
            Arrangement::DistributeHorizontally => "distribute_horizontally",
            Arrangement::DistributeVertically => "distribute_vertically",
            Arrangement::SmallestWidth => "resize_to_smallest_width",
            Arrangement::SmallestHeight => "resize_to_smallest_height",
            Arrangement::SmallestWidthAndHeight => "resize_to_smallest_width_and_height",
            Arrangement::LargestWidth => "resize_to_largest_width",
            Arrangement::LargestHeight => "resize_to_largest_height",
            Arrangement::LargestWidthAndHeight => "resize_to_largest_width_and_height",
        }
    }

    pub fn from_id(id: &str) -> Option<Arrangement> {
        Arrangement::ALL.into_iter().find(|a| a.id() == id)
    }

    /// The Arrange menu's submenu and item. The item names follow the
    /// help's wording; FileMaker Pro's own are unmeasured (client-ui.md).
    pub fn menu(self) -> (&'static str, &'static str) {
        match self {
            Arrangement::AlignLeftEdges => ("Align", "Left Edges"),
            Arrangement::AlignVerticalCenters => ("Align", "Vertical Centers"),
            Arrangement::AlignRightEdges => ("Align", "Right Edges"),
            Arrangement::AlignTopEdges => ("Align", "Top Edges"),
            Arrangement::AlignHorizontalCenters => ("Align", "Horizontal Centers"),
            Arrangement::AlignBottomEdges => ("Align", "Bottom Edges"),
            Arrangement::DistributeHorizontally => ("Distribute", "Horizontally"),
            Arrangement::DistributeVertically => ("Distribute", "Vertically"),
            Arrangement::SmallestWidth => ("Resize To", "Smallest Width"),
            Arrangement::SmallestHeight => ("Resize To", "Smallest Height"),
            Arrangement::SmallestWidthAndHeight => ("Resize To", "Smallest Width and Height"),
            Arrangement::LargestWidth => ("Resize To", "Largest Width"),
            Arrangement::LargestHeight => ("Resize To", "Largest Height"),
            Arrangement::LargestWidthAndHeight => ("Resize To", "Largest Width and Height"),
        }
    }

    /// The fewest objects it applies to: two, and three to distribute
    /// (inferred: two objects have nothing between them).
    pub fn minimum(self) -> usize {
        match self {
            Arrangement::DistributeHorizontally | Arrangement::DistributeVertically => 3,
            _ => 2,
        }
    }
}

/// Whether `frame` is one an arrangement takes: finite edges, no negative
/// size, and a size in at least one direction (a line has none in the
/// other), as the frames `layout_objects::plan` writes.
pub fn arrangeable(frame: &Rect) -> bool {
    let finite = [frame.left, frame.top, frame.right, frame.bottom]
        .iter()
        .all(|v| v.is_finite());
    finite
        && width(frame) >= 0.0
        && height(frame) >= 0.0
        && (width(frame) > 0.0 || height(frame) > 0.0)
}

/// The frames `arrangement` gives `objects`: only those it changes, in the
/// objects' order. Fewer objects than [`Arrangement::minimum`] change
/// nothing; a frame that is not [`arrangeable`] is refused. Left and top
/// edges land on whole points (and never below 0); widths and heights
/// change only with Resize To.
pub fn arrange(
    objects: &[ObjectFrame],
    arrangement: Arrangement,
) -> Result<Vec<ObjectFrame>, Refusal> {
    if let Some(bad) = objects.iter().find(|o| !arrangeable(&o.frame)) {
        return Err(Refusal::Invalid(format!(
            "Object {}'s frame has no size or is not a frame; it cannot be arranged.",
            bad.object
        )));
    }
    if objects.len() < arrangement.minimum() {
        return Ok(Vec::new());
    }
    let frames: Vec<&Rect> = objects.iter().map(|o| &o.frame).collect();
    let least =
        |edge: fn(&Rect) -> f64| frames.iter().map(|r| edge(r)).fold(f64::INFINITY, f64::min);
    let most = |edge: fn(&Rect) -> f64| {
        frames
            .iter()
            .map(|r| edge(r))
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let new: Vec<Rect> = match arrangement {
        Arrangement::AlignLeftEdges => {
            let x = least(|r| r.left);
            frames.iter().map(|r| at(r, x, r.top)).collect()
        }
        Arrangement::AlignRightEdges => {
            let x = most(|r| r.right);
            frames.iter().map(|r| at(r, x - width(r), r.top)).collect()
        }
        Arrangement::AlignVerticalCenters => {
            let x = (least(|r| r.left) + most(|r| r.right)) / 2.0;
            frames
                .iter()
                .map(|r| at(r, x - width(r) / 2.0, r.top))
                .collect()
        }
        Arrangement::AlignTopEdges => {
            let y = least(|r| r.top);
            frames.iter().map(|r| at(r, r.left, y)).collect()
        }
        Arrangement::AlignBottomEdges => {
            let y = most(|r| r.bottom);
            frames
                .iter()
                .map(|r| at(r, r.left, y - height(r)))
                .collect()
        }
        Arrangement::AlignHorizontalCenters => {
            let y = (least(|r| r.top) + most(|r| r.bottom)) / 2.0;
            frames
                .iter()
                .map(|r| at(r, r.left, y - height(r) / 2.0))
                .collect()
        }
        Arrangement::DistributeHorizontally => distribute(&frames, true),
        Arrangement::DistributeVertically => distribute(&frames, false),
        Arrangement::SmallestWidth
        | Arrangement::SmallestHeight
        | Arrangement::SmallestWidthAndHeight
        | Arrangement::LargestWidth
        | Arrangement::LargestHeight
        | Arrangement::LargestWidthAndHeight => {
            let largest = matches!(
                arrangement,
                Arrangement::LargestWidth
                    | Arrangement::LargestHeight
                    | Arrangement::LargestWidthAndHeight
            );
            let pick = |size: fn(&Rect) -> f64| {
                let sizes = frames.iter().map(|r| size(r));
                if largest {
                    sizes.fold(f64::NEG_INFINITY, f64::max)
                } else {
                    sizes.fold(f64::INFINITY, f64::min)
                }
            };
            let w = (!matches!(
                arrangement,
                Arrangement::SmallestHeight | Arrangement::LargestHeight
            ))
            .then(|| pick(width));
            let h = (!matches!(
                arrangement,
                Arrangement::SmallestWidth | Arrangement::LargestWidth
            ))
            .then(|| pick(height));
            frames
                .iter()
                .map(|r| {
                    let mut new = at(r, r.left, r.top);
                    if let Some(w) = w {
                        new.right = new.left + w;
                    }
                    if let Some(h) = h {
                        new.bottom = new.top + h;
                    }
                    new
                })
                .collect()
        }
    };
    Ok(objects
        .iter()
        .zip(new)
        .filter(|(old, new)| old.frame != *new)
        .map(|(old, frame)| ObjectFrame {
            object: old.object,
            frame,
        })
        .collect())
}

fn width(r: &Rect) -> f64 {
    r.right - r.left
}

fn height(r: &Rect) -> f64 {
    r.bottom - r.top
}

/// `r` with its left and top edges at `left`, `top` on whole points (not
/// below 0), its size kept.
fn at(r: &Rect, left: f64, top: f64) -> Rect {
    let (left, top) = (left.round().max(0.0), top.round().max(0.0));
    Rect {
        left,
        top,
        right: left + width(r),
        bottom: top + height(r),
        other: r.other.clone(),
    }
}

/// Equal gaps between neighbours along one axis: the objects in order of
/// their leading edge (top or left, then their place in `frames`), the
/// first and the last staying exactly where they are. When the objects do
/// not fit between the outermost edges, equal gaps would be negative and
/// could reverse their order; their leading edges are spaced evenly
/// instead.
fn distribute(frames: &[&Rect], horizontal: bool) -> Vec<Rect> {
    let lead = |r: &Rect| if horizontal { r.left } else { r.top };
    let size = |r: &Rect| if horizontal { width(r) } else { height(r) };
    let mut order: Vec<usize> = (0..frames.len()).collect();
    order.sort_by(|&a, &b| lead(frames[a]).total_cmp(&lead(frames[b])).then(a.cmp(&b)));
    let (first, last) = (order[0], order[order.len() - 1]);
    let (start, end) = (lead(frames[first]), lead(frames[last]));
    let span = end + size(frames[last]) - start;
    let total: f64 = frames.iter().map(|r| size(r)).sum();
    let steps = (frames.len() - 1) as f64;
    let gap = (span - total) / steps;
    let mut leads = vec![0.0; frames.len()];
    let mut next = start;
    for (n, &i) in order.iter().enumerate() {
        leads[i] = if gap >= 0.0 {
            next
        } else {
            start + (end - start) * n as f64 / steps
        };
        next += size(frames[i]) + gap;
    }
    frames
        .iter()
        .zip(leads)
        .enumerate()
        .map(|(i, (r, to))| {
            if i == first || i == last {
                (*r).clone()
            } else if horizontal {
                at(r, to, r.top)
            } else {
                at(r, r.left, to)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(object: u64, left: f64, top: f64, width: f64, height: f64) -> ObjectFrame {
        ObjectFrame {
            object,
            frame: Rect {
                left,
                top,
                right: left + width,
                bottom: top + height,
                other: Vec::new(),
            },
        }
    }

    /// Each arrangement on the same three objects: what each object's
    /// frame becomes (left, top, width, height), and which change.
    #[test]
    fn arrangements_move_and_resize_relative_to_one_another() {
        let objects = [
            frame(1, 10.0, 40.0, 100.0, 20.0),
            frame(2, 30.0, 10.0, 60.0, 30.0),
            frame(3, 200.0, 100.0, 40.0, 10.0),
        ];
        // An arrangement, and each object's left, top, width and height
        // after it.
        type Row = (Arrangement, [(f64, f64, f64, f64); 3]);
        let rows: [Row; 14] = [
            (
                Arrangement::AlignLeftEdges,
                [
                    (10., 40., 100., 20.),
                    (10., 10., 60., 30.),
                    (10., 100., 40., 10.),
                ],
            ),
            // The extent is 10..240: its middle 125.
            (
                Arrangement::AlignVerticalCenters,
                [
                    (75., 40., 100., 20.),
                    (95., 10., 60., 30.),
                    (105., 100., 40., 10.),
                ],
            ),
            (
                Arrangement::AlignRightEdges,
                [
                    (140., 40., 100., 20.),
                    (180., 10., 60., 30.),
                    (200., 100., 40., 10.),
                ],
            ),
            (
                Arrangement::AlignTopEdges,
                [
                    (10., 10., 100., 20.),
                    (30., 10., 60., 30.),
                    (200., 10., 40., 10.),
                ],
            ),
            // The extent is 10..110: its middle 60.
            (
                Arrangement::AlignHorizontalCenters,
                [
                    (10., 50., 100., 20.),
                    (30., 45., 60., 30.),
                    (200., 55., 40., 10.),
                ],
            ),
            (
                Arrangement::AlignBottomEdges,
                [
                    (10., 90., 100., 20.),
                    (30., 80., 60., 30.),
                    (200., 100., 40., 10.),
                ],
            ),
            // 10..240 holds 200 points of objects: two gaps of 15, the
            // middle object (by left edge, 2) moves to 125.
            (
                Arrangement::DistributeHorizontally,
                [
                    (10., 40., 100., 20.),
                    (125., 10., 60., 30.),
                    (200., 100., 40., 10.),
                ],
            ),
            // In top order 2, 1, 3; 10..110 holds 60 points: gaps of 20.
            (
                Arrangement::DistributeVertically,
                [
                    (10., 60., 100., 20.),
                    (30., 10., 60., 30.),
                    (200., 100., 40., 10.),
                ],
            ),
            (
                Arrangement::SmallestWidth,
                [
                    (10., 40., 40., 20.),
                    (30., 10., 40., 30.),
                    (200., 100., 40., 10.),
                ],
            ),
            (
                Arrangement::SmallestHeight,
                [
                    (10., 40., 100., 10.),
                    (30., 10., 60., 10.),
                    (200., 100., 40., 10.),
                ],
            ),
            (
                Arrangement::SmallestWidthAndHeight,
                [
                    (10., 40., 40., 10.),
                    (30., 10., 40., 10.),
                    (200., 100., 40., 10.),
                ],
            ),
            (
                Arrangement::LargestWidth,
                [
                    (10., 40., 100., 20.),
                    (30., 10., 100., 30.),
                    (200., 100., 100., 10.),
                ],
            ),
            (
                Arrangement::LargestHeight,
                [
                    (10., 40., 100., 30.),
                    (30., 10., 60., 30.),
                    (200., 100., 40., 30.),
                ],
            ),
            (
                Arrangement::LargestWidthAndHeight,
                [
                    (10., 40., 100., 30.),
                    (30., 10., 100., 30.),
                    (200., 100., 100., 30.),
                ],
            ),
        ];
        for (arrangement, wanted) in rows {
            let changed = arrange(&objects, arrangement).unwrap();
            for (object, (left, top, width, height)) in objects.iter().zip(wanted) {
                let expected = frame(object.object, left, top, width, height);
                let got = changed.iter().find(|f| f.object == object.object);
                match got {
                    Some(got) => assert_eq!(got, &expected, "{arrangement:?}"),
                    None => assert_eq!(object, &expected, "{arrangement:?}: unchanged"),
                }
            }
            assert_eq!(Arrangement::from_id(arrangement.id()), Some(arrangement));
        }
        // Fractional results land on whole points; too few objects change
        // nothing.
        let odd = [
            frame(1, 0.0, 0.0, 15.0, 5.0),
            frame(2, 20.0, 0.0, 10.0, 5.0),
        ];
        let centered = arrange(&odd, Arrangement::AlignVerticalCenters).unwrap();
        assert_eq!(centered[0].frame.left, 8.0);
        assert!(arrange(&odd, Arrangement::DistributeHorizontally)
            .unwrap()
            .is_empty());
        assert!(arrange(&odd[..1], Arrangement::AlignLeftEdges)
            .unwrap()
            .is_empty());
        // The outermost objects keep their frames exactly, fractions
        // included.
        let fractional = [
            frame(1, 0.0, 0.0, 10.0, 5.0),
            frame(2, 15.0, 0.0, 10.0, 5.0),
            frame(3, 40.5, 0.0, 10.0, 5.0),
        ];
        let spaced = arrange(&fractional, Arrangement::DistributeHorizontally).unwrap();
        assert!(spaced.iter().all(|f| f.object == 2), "{spaced:?}");
        // Too wide to fit: leading edges evenly spaced, order kept, and the
        // same again when repeated.
        let wide = [
            frame(1, 100.0, 0.0, 10.0, 5.0),
            frame(2, 110.0, 20.0, 70.0, 5.0),
            frame(3, 130.0, 40.0, 10.0, 5.0),
        ];
        let once = arrange(&wide, Arrangement::DistributeHorizontally).unwrap();
        assert_eq!(once, [frame(2, 115.0, 20.0, 70.0, 5.0)]);
        let again = [wide[0].clone(), once[0].clone(), wide[2].clone()];
        assert!(arrange(&again, Arrangement::DistributeHorizontally)
            .unwrap()
            .is_empty());
        // A frame that is not a frame is refused, not taken as unchanged or
        // as an anchor.
        let mut bad = odd.clone();
        bad[0].frame.left = f64::NAN;
        assert!(arrange(&bad, Arrangement::AlignLeftEdges).is_err());
        bad[0].frame = Rect {
            left: 10.0,
            right: 5.0,
            ..odd[0].frame.clone()
        };
        assert!(arrange(&bad, Arrangement::AlignLeftEdges).is_err());
        let line = [
            frame(1, 0.0, 0.0, 0.0, 30.0),
            frame(2, 20.0, 0.0, 10.0, 5.0),
        ];
        assert_eq!(
            arrange(&line, Arrangement::AlignLeftEdges).unwrap().len(),
            1
        );
    }
}
