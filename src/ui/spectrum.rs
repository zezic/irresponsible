use super::chrome::{Align, Face, Painter, opacity};
use irresponsible::{
    appearance::Appearance,
    spectrum::{Analyzer, Settings, Source},
};
use vello::{
    kurbo::{Affine, BezPath, CubicBez, Point, Rect},
    peniko::{BlendMode, Compose, Fill, Mix},
};

pub fn draw<A: Copy + PartialEq>(
    p: &mut Painter<'_, A>,
    analyzer: &Analyzer,
    settings: Settings,
    plot: Rect,
    max_hz: f32,
    scale: f64,
) {
    if !settings.enabled || settings.intensity.alpha() == 0.0 || !analyzer.ready() {
        return;
    }
    let color = p.colors.basic[6];
    let floor = settings.range.floor();
    let mut points: Vec<Point> = Vec::with_capacity(plot.width().ceil() as usize + 2);
    // Keep peak levels at their actual positions when bins share a log-axis column.
    // Retain neighboring bins outside the plot so clipping does not taper its edges.
    for (hz, db) in analyzer.bins().skip(1) {
        let x = plot.x0 + f64::from((hz / 20.0).ln() / (max_hz / 20.0).ln()) * plot.width();
        let y = plot.y1 - f64::from(((db - floor) / -floor).clamp(0.0, 1.0)) * plot.height();
        match points.last_mut() {
            Some(peak) if peak.x.floor() == x.floor() => {
                if y < peak.y {
                    *peak = Point::new(x, y);
                }
            }
            _ => points.push(Point::new(x, y)),
        }
        if hz >= max_hz {
            break;
        }
    }
    let first = points
        .first()
        .expect("a complete FFT has positive-frequency bins");
    let last = points
        .last()
        .expect("a complete FFT has positive-frequency bins");
    let mut path = BezPath::new();
    path.move_to((first.x, plot.y1));
    path.line_to(*first);
    for curve in smooth_segments(&points) {
        path.curve_to(curve.p1, curve.p2, curve.p3);
    }
    path.line_to((last.x, plot.y1));
    path.close_path();
    let transform = Affine::scale(scale);
    p.scene.push_layer(
        Fill::NonZero,
        blend_mode(p.colors.cast()),
        1.0,
        transform,
        &plot,
    );
    p.scene.fill(
        Fill::NonZero,
        transform,
        opacity(color, settings.intensity.alpha()),
        None,
        &path,
    );
    p.scene.pop_layer();
    p.scene.push_clip_layer(Fill::NonZero, transform, &plot);
    let label = format!(
        "{} spectrum / {floor:.0} to 0 dBFS",
        match settings.source {
            Source::Input => "Input",
            Source::Output => "Output",
        }
    );
    p.text(
        &label,
        Rect::new(plot.x0 + 6.0, plot.y1 - 22.0, plot.x1 - 42.0, plot.y1 - 2.0),
        Face::Small,
        Align::Start,
        p.colors.secondary,
    );
    for db in [0.0, floor * 0.5, floor] {
        let y = plot.y0 + f64::from(db / floor) * plot.height();
        p.text(
            &format!("{db:.0}"),
            Rect::new(
                plot.x1 - 36.0,
                (y - 10.0).clamp(plot.y0, plot.y1 - 20.0),
                plot.x1 - 4.0,
                (y + 10.0).clamp(plot.y0 + 20.0, plot.y1),
            ),
            Face::SmallValue,
            Align::End,
            opacity(color, 0.65),
        );
    }
    p.scene.pop_layer();
}

fn blend_mode(appearance: Appearance) -> BlendMode {
    match appearance {
        Appearance::Dark => Compose::PlusLighter.into(),
        Appearance::Light => Mix::Multiply.into(),
    }
}

/// Shape-preserving cubic Hermite interpolation in screen (log-frequency) space.
/// Slopes follow the PCHIP weighted harmonic mean and one-sided endpoint rule:
/// https://docs.scipy.org/doc/scipy/reference/generated/scipy.interpolate.PchipInterpolator.html
fn smooth_segments(points: &[Point]) -> impl Iterator<Item = CubicBez> + '_ {
    assert!(
        points.len() >= 2,
        "a spectrum outline needs at least two distinct columns"
    );
    let mut slope = tangent(points, 0);
    points.windows(2).enumerate().map(move |(i, pair)| {
        let next = tangent(points, i + 1);
        let third = (pair[1].x - pair[0].x) / 3.0;
        let curve = CubicBez::new(
            pair[0],
            Point::new(pair[0].x + third, pair[0].y + slope * third),
            Point::new(pair[1].x - third, pair[1].y - next * third),
            pair[1],
        );
        slope = next;
        curve
    })
}
fn interval(a: Point, b: Point) -> (f64, f64) {
    let width = b.x - a.x;
    assert!(width > 0.0, "spectrum columns are strictly ordered");
    (width, (b.y - a.y) / width)
}
fn tangent(points: &[Point], i: usize) -> f64 {
    if points.len() == 2 {
        return interval(points[0], points[1]).1;
    }
    if i == 0 || i == points.len() - 1 {
        let ((width, slope), (neighbor_width, neighbor_slope)) = if i == 0 {
            (
                interval(points[0], points[1]),
                interval(points[1], points[2]),
            )
        } else {
            (
                interval(points[i - 1], points[i]),
                interval(points[i - 2], points[i - 1]),
            )
        };
        let end = ((2.0 * width + neighbor_width) * slope - width * neighbor_slope)
            / (width + neighbor_width);
        return if end.signum() != slope.signum() {
            0.0
        } else if slope.signum() != neighbor_slope.signum() && end.abs() > 3.0 * slope.abs() {
            3.0 * slope
        } else {
            end
        };
    }
    let (left_width, left) = interval(points[i - 1], points[i]);
    let (right_width, right) = interval(points[i], points[i + 1]);
    if left == 0.0 || right == 0.0 || left.signum() != right.signum() {
        return 0.0;
    }
    let w1 = 2.0 * right_width + left_width;
    let w2 = right_width + 2.0 * left_width;
    (w1 + w2) / (w1 / left + w2 / right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::{ParamCurve, ParamCurveDeriv};
    #[test]
    fn nonuniform_spectrum_curves_preserve_levels_and_slopes_without_overshoot() {
        let points = [
            Point::new(-40.0, 100.0),
            Point::new(0.0, 100.0),
            Point::new(55.0, 50.0),
            Point::new(80.0, 5.0),
            Point::new(95.0, 45.0),
            Point::new(98.0, 100.0),
            Point::new(99.0, 100.0),
            Point::new(100.0, 15.0),
            Point::new(101.0, 100.0),
        ];
        let curves: Vec<_> = smooth_segments(&points).collect();
        for (curve, pair) in curves.iter().zip(points.windows(2)) {
            assert_eq!(curve.p0, pair[0]);
            assert_eq!(curve.p3, pair[1]);
            for step in 0..=100 {
                let point = curve.eval(f64::from(step) / 100.0);
                assert!(
                    point.y >= pair[0].y.min(pair[1].y) - 1e-10
                        && point.y <= pair[0].y.max(pair[1].y) + 1e-10,
                    "interpolation must not invent a peak: {point:?}"
                );
                assert!(point.x >= pair[0].x - 1e-10 && point.x <= pair[1].x + 1e-10);
            }
        }
        for pair in curves.windows(2) {
            let left = pair[0].deriv().eval(1.0);
            let right = pair[1].deriv().eval(0.0);
            assert!(
                (left.y / left.x - right.y / right.x).abs() < 1e-10,
                "slopes must meet smoothly even across unequal bin widths"
            );
        }
        let reference = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(2.0, 1.5),
            Point::new(4.0, 0.0),
        ];
        let first = smooth_segments(&reference).next().unwrap();
        assert!((first.eval(0.5).y - 0.5729166666666666).abs() < 1e-12);
    }
}
