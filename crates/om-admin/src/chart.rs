//! Charts drawn on the server as SVG, so the console needs no charting
//! library (`docs/admin-console.md`, "Rendering"): the Dashboard's line
//! chart of system statistics and its Volume Status doughnut, at the sizes
//! of FileMaker Server's (`docs/re/admin-console.md` 4.1). Colors and type
//! come from the style sheet through class names: the CSP forbids inline
//! style attributes.

use maud::{html, Markup};

/// The line chart's drawing, in px.
const WIDTH: f64 = 680.0;
const HEIGHT: f64 = 280.0;
const PLOT_TOP: f64 = 36.0;
const PLOT_BOTTOM: f64 = 212.0;
const PLOT_RIGHT: f64 = WIDTH - 30.0;
/// The y axis's title is centered this far from the left.
const Y_TITLE_X: f64 = 12.0;
/// Tick marks stick out of the plot this far.
const TICK: f64 = 6.0;
/// The most intervals the y axis is divided into.
const MAX_INTERVALS: f64 = 8.0;

/// A line chart of one series over evenly spaced points.
pub(crate) struct LineChart<'a> {
    pub(crate) title: &'a str,
    /// The y axis's title, such as "Percentage".
    pub(crate) y_title: &'a str,
    /// The x axis's title, such as "Time".
    pub(crate) x_title: &'a str,
    /// One label per point.
    pub(crate) labels: &'a [String],
    /// One value per point; `None` leaves a gap in the line.
    pub(crate) values: &'a [Option<f64>],
}

/// The y axis: `intervals` steps of `step` from zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Scale {
    pub(crate) step: f64,
    pub(crate) intervals: usize,
    /// Decimals the tick labels need.
    pub(crate) decimals: usize,
}

impl Scale {
    /// The axis for values up to `max`: from zero to the first multiple of
    /// a round step (1, 2 or 5 times a power of ten) at or above `max`, in
    /// at most eight intervals. Values of zero (or none) get an axis to 1.
    pub(crate) fn for_max(max: f64) -> Scale {
        let max = if max.is_finite() && max > 0.0 {
            max
        } else {
            1.0
        };
        let raw = max / MAX_INTERVALS;
        let power = raw.log10().floor();
        let magnitude = 10f64.powf(power);
        let multiple = [1.0, 2.0, 5.0, 10.0]
            .into_iter()
            .find(|m| m * magnitude >= raw * (1.0 - 1e-9))
            .unwrap_or(10.0);
        let step = multiple * magnitude;
        // The steps are exact in decimal: 1, 2 or 5 times a power of ten.
        let decimals = if multiple >= 10.0 {
            -power - 1.0
        } else {
            -power
        }
        .max(0.0);
        let intervals = ((max / step) * (1.0 - 1e-9)).ceil().max(1.0);
        Scale {
            step,
            intervals: intervals as usize,
            decimals: decimals as usize,
        }
    }

    pub(crate) fn top(&self) -> f64 {
        self.step * self.intervals as f64
    }

    /// Tick `i`'s label, with thousands separators and no trailing
    /// zeros: `160,000`, `0.5`, `1`.
    pub(crate) fn label(&self, i: usize) -> String {
        let text = format!("{:.*}", self.decimals, self.step * i as f64);
        let (whole, fraction) = match text.split_once('.') {
            Some((whole, fraction)) => (whole, fraction.trim_end_matches('0')),
            None => (text.as_str(), ""),
        };
        let mut grouped = String::new();
        for (n, digit) in whole.chars().enumerate() {
            if n > 0 && (whole.len() - n) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(digit);
        }
        if fraction.is_empty() {
            grouped
        } else {
            format!("{grouped}.{fraction}")
        }
    }
}

/// The width of `text` at the chart's 12 px, as the axis lays it out:
/// digits are 0.556 em in Helvetica and Arial, separators 0.278 em.
fn label_width(text: &str) -> f64 {
    text.chars()
        .map(|c| if c.is_ascii_digit() { 6.67 } else { 3.34 })
        .sum()
}

fn fmt(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    format!("{rounded}")
}

/// The chart as an SVG of 680x280 px that scales down with its column.
pub(crate) fn line_chart(chart: &LineChart) -> Markup {
    let max = chart
        .values
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0, f64::max);
    let scale = Scale::for_max(max);
    let ticks: Vec<String> = (0..=scale.intervals).map(|i| scale.label(i)).collect();
    let widest = ticks.iter().map(|t| label_width(t)).fold(0.0, f64::max);
    let left = (32.0 + widest + 15.0).round();
    let points = chart.labels.len().max(chart.values.len());
    let x = |i: usize| -> f64 {
        if points <= 1 {
            (left + PLOT_RIGHT) / 2.0
        } else {
            left + (PLOT_RIGHT - left) * i as f64 / (points - 1) as f64
        }
    };
    let y = |value: f64| -> f64 {
        let value = value.clamp(0.0, scale.top());
        PLOT_BOTTOM - (PLOT_BOTTOM - PLOT_TOP) * value / scale.top()
    };
    let center = (left + PLOT_RIGHT) / 2.0;
    let runs = runs(chart.values, &x, &y);
    html! {
        svg.chart viewBox={"0 0 " (WIDTH) " " (HEIGHT)} width=(WIDTH) height=(HEIGHT)
            role="img" aria-label=(chart.title) {
            title { (chart.title) }
            text.chart-title x=(WIDTH / 2.0) y="22" text-anchor="middle" { (chart.title) }
            // The grid, its tick marks and labels.
            @for (i, tick) in ticks.iter().enumerate() {
                @let ty = fmt(y(scale.step * i as f64));
                line.chart-grid x1=(fmt(left - TICK)) y1=(ty) x2=(PLOT_RIGHT) y2=(ty) {}
                text.chart-tick x=(fmt(left - TICK - 4.0)) y=(ty) dy="4" text-anchor="end" { (tick) }
            }
            @for i in 0..points {
                @let tx = fmt(x(i));
                line.chart-grid x1=(tx) y1=(PLOT_TOP) x2=(tx) y2=(PLOT_BOTTOM + TICK) {}
                @if let Some(label) = chart.labels.get(i) {
                    text.chart-tick x=(tx) y=(PLOT_BOTTOM + 21.0) text-anchor="middle" { (label) }
                }
            }
            // The plot's edges, where no point's line draws them.
            @if points <= 1 {
                line.chart-grid x1=(left) y1=(PLOT_TOP) x2=(left) y2=(PLOT_BOTTOM + TICK) {}
                line.chart-grid x1=(PLOT_RIGHT) y1=(PLOT_TOP) x2=(PLOT_RIGHT) y2=(PLOT_BOTTOM) {}
            }
            text.chart-axis-title x=(fmt(center)) y=(HEIGHT - 12.0) text-anchor="middle" {
                (chart.x_title)
            }
            text.chart-axis-title text-anchor="middle"
                transform={"translate(" (Y_TITLE_X) " " (fmt((PLOT_TOP + PLOT_BOTTOM) / 2.0)) ") rotate(-90)"} {
                (chart.y_title)
            }
            // The series: a faint fill under a smooth line, and its points.
            @for run in &runs {
                @if run.len() > 1 {
                    path.chart-fill d=(area(run)) {}
                    path.chart-line d=(curve(run)) {}
                }
            }
            @for run in &runs {
                @for (px, py) in run {
                    circle.chart-point cx=(fmt(*px)) cy=(fmt(*py)) r="2.75" {}
                }
            }
        }
    }
}

/// The values' points in px, split where a value is missing.
fn runs(
    values: &[Option<f64>],
    x: &impl Fn(usize) -> f64,
    y: &impl Fn(f64) -> f64,
) -> Vec<Vec<(f64, f64)>> {
    let mut runs = vec![Vec::new()];
    for (i, value) in values.iter().enumerate() {
        match value.filter(|v| v.is_finite()) {
            Some(v) => {
                if let Some(run) = runs.last_mut() {
                    run.push((x(i), y(v)));
                }
            }
            None => runs.push(Vec::new()),
        }
    }
    runs.retain(|r| !r.is_empty());
    runs
}

/// A smooth line through `points` that never overshoots them (monotone
/// cubic interpolation, Fritsch and Carlson), so a series of zeros and
/// small values never dips below the axis.
fn curve(points: &[(f64, f64)]) -> String {
    let n = points.len();
    let mut d = format!("M{} {}", fmt(points[0].0), fmt(points[0].1));
    if n < 2 {
        return d;
    }
    let slopes: Vec<f64> = points
        .windows(2)
        .map(|w| (w[1].1 - w[0].1) / (w[1].0 - w[0].0))
        .collect();
    let mut tangents = vec![0.0; n];
    tangents[0] = slopes[0];
    tangents[n - 1] = slopes[n - 2];
    for i in 1..n - 1 {
        tangents[i] = if slopes[i - 1] * slopes[i] <= 0.0 {
            0.0
        } else {
            (slopes[i - 1] + slopes[i]) / 2.0
        };
    }
    for i in 0..n - 1 {
        if slopes[i] == 0.0 {
            tangents[i] = 0.0;
            tangents[i + 1] = 0.0;
            continue;
        }
        let a = tangents[i] / slopes[i];
        let b = tangents[i + 1] / slopes[i];
        let h = a * a + b * b;
        if h > 9.0 {
            let t = 3.0 / h.sqrt();
            tangents[i] = t * a * slopes[i];
            tangents[i + 1] = t * b * slopes[i];
        }
    }
    for i in 0..n - 1 {
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        let third = (x1 - x0) / 3.0;
        d.push_str(&format!(
            " C{} {} {} {} {} {}",
            fmt(x0 + third),
            fmt(y0 + tangents[i] * third),
            fmt(x1 - third),
            fmt(y1 - tangents[i + 1] * third),
            fmt(x1),
            fmt(y1)
        ));
    }
    d
}

/// The area under the curve through `points`, down to the x axis.
fn area(points: &[(f64, f64)]) -> String {
    let (first, last) = (points[0].0, points[points.len() - 1].0);
    format!(
        "{} L{} {} L{} {} Z",
        curve(points),
        fmt(last),
        fmt(PLOT_BOTTOM),
        fmt(first),
        fmt(PLOT_BOTTOM)
    )
}

/// A doughnut with `fraction` (0 to 1) of its ring in the accent color,
/// clockwise from the top, and the rest light gray; 180 px across.
pub(crate) fn doughnut(fraction: f64) -> Markup {
    const RADIUS: f64 = 76.0;
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let circumference = 2.0 * std::f64::consts::PI * RADIUS;
    let used = circumference * fraction;
    html! {
        svg.doughnut-ring viewBox="0 0 180 180" width="180" height="180" aria-hidden="true"
            focusable="false" {
            circle.doughnut-free cx="90" cy="90" r=(RADIUS) {}
            @if fraction > 0.0 {
                circle.doughnut-used cx="90" cy="90" r=(RADIUS) transform="rotate(-90 90 90)"
                    stroke-dasharray={(fmt(used)) " " (fmt(circumference))} {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_y_axis_ends_on_a_round_step_above_the_largest_value() {
        // As FileMaker Server's charts show: 6.7 % on 0-7 by 1, 14.3 % on
        // 0-16 by 2.
        let cpu = Scale::for_max(6.7);
        assert_eq!(
            (cpu.step, cpu.intervals, cpu.label(7)),
            (1.0, 7, "7".into())
        );
        let memory = Scale::for_max(14.3);
        assert_eq!((memory.step, memory.intervals), (2.0, 8));
        let network = Scale::for_max(42_000.0);
        assert_eq!((network.step, network.intervals), (10_000.0, 5));
        assert_eq!(network.label(5), "50,000");
        assert_eq!(Scale::for_max(160_000.0).label(8), "160,000");
        // Exactly on a step, and at the most intervals.
        let full = Scale::for_max(100.0);
        assert_eq!((full.step, full.intervals, full.top()), (20.0, 5, 100.0));
        assert_eq!(Scale::for_max(8.0).intervals, 8);
        // Small rates get decimals; nothing at all gets an axis to 1.
        let small = Scale::for_max(0.03);
        assert_eq!((small.label(1), small.intervals), ("0.005".into(), 6));
        let none = Scale::for_max(0.0);
        assert_eq!(
            (none.label(0), none.label(1), none.label(5)),
            ("0".into(), "0.2".into(), "1".into())
        );
        assert_eq!(Scale::for_max(f64::NAN).intervals, 5);
    }

    #[test]
    fn the_line_never_leaves_the_plot() {
        let points = [(0.0, 212.0), (10.0, 212.0), (20.0, 40.0), (30.0, 212.0)];
        let d = curve(&points);
        let numbers: Vec<f64> = d
            .split([' ', 'M', 'C'])
            .filter_map(|n| n.parse().ok())
            .collect();
        for pair in numbers.chunks(2) {
            assert!((40.0..=212.0).contains(&pair[1]), "{d}");
        }
    }
}
