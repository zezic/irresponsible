use vello::kurbo::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrollbar {
    pub rail: Rect,
    pub thumb: Rect,
    reach: f64,
}
impl Scrollbar {
    pub fn new(viewport: Rect, extent: f64, offset: f64) -> Option<Self> {
        let reach = extent - viewport.height();
        if reach <= 0.0 {
            return None;
        }
        assert!(
            viewport.height() > 26.0,
            "a scrolling viewport must leave room for its thumb"
        );
        let rail = Rect::new(
            viewport.x1 - 12.0,
            viewport.y0 + 5.0,
            viewport.x1,
            viewport.y1 - 5.0,
        );
        let height = (rail.height() * viewport.height() / extent)
            .max(16.0)
            .min(rail.height());
        let y = rail.y0 + (rail.height() - height) * (offset / reach).clamp(0.0, 1.0);
        Some(Self {
            rail,
            thumb: Rect::new(rail.x1 - 7.0, y, rail.x1 - 3.0, y + height),
            reach,
        })
    }
    pub fn grab(self, point: Point) -> f64 {
        if (self.thumb.y0..=self.thumb.y1).contains(&point.y) {
            point.y - self.thumb.y0
        } else {
            self.thumb.height() * 0.5
        }
    }
    pub fn offset(self, y: f64, grab: f64) -> f64 {
        ((y - self.rail.y0 - grab) / (self.rail.height() - self.thumb.height())).clamp(0.0, 1.0)
            * self.reach
    }
}
