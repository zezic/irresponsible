use super::{
    chrome::{CONTROL, GAP, INSET, LIST_ROW_HEIGHT, ROW, RULE, SIDEBAR},
    window_chrome::WindowChrome,
};
use vello::kurbo::{Rect, Size};

pub const MIN_HEIGHT: f64 = 760.0 + Header::STRIP;
const SECTION_TOP: f64 = 12.0;
const SECTION_GAP: f64 = 7.0;
const SECTION_BOTTOM: f64 = 8.0;

#[derive(Clone, Copy)]
pub struct GraphLayout {
    pub graph: Rect,
    pub plot: Rect,
    pub legend: Rect,
    pub toast: Option<Rect>,
}
impl GraphLayout {
    fn new(graph: Rect, candidate: bool) -> Self {
        let toast = candidate.then(|| {
            Rect::new(
                graph.x0 + GAP,
                graph.y0 + GAP,
                graph.x1 - GAP,
                graph.y0 + GAP + 48.0,
            )
        });
        let legend = Rect::new(
            graph.x0 + INSET,
            graph.y1 - INSET - CONTROL,
            graph.x1 - INSET,
            graph.y1 - INSET,
        );
        let plot = Rect::new(
            graph.x0 + 36.0,
            graph.y0 + INSET,
            graph.x1 - INSET,
            legend.y0 - 24.0,
        );
        Self {
            graph,
            plot,
            legend,
            toast,
        }
    }
    pub const ROOM_CHROME_HEIGHT: f64 = CONTROL + INSET + CONTROL + GAP + CONTROL;
    pub fn room(bounds: Rect, y: f64, plot_height: f64) -> Self {
        let plot = Rect::new(
            bounds.x0 + 36.0,
            y + CONTROL + INSET,
            bounds.x1,
            y + CONTROL + INSET + plot_height,
        );
        let legend = Rect::new(
            bounds.x0,
            plot.y1 + CONTROL + GAP,
            bounds.x1,
            plot.y1 + CONTROL + GAP + CONTROL,
        );
        Self {
            graph: Rect::new(bounds.x0, y, bounds.x1, legend.y1),
            plot,
            legend,
            toast: None,
        }
    }
    pub fn compact(size: Size, chrome: WindowChrome, candidate: bool) -> Self {
        Self::new(
            Rect::new(0.0, chrome.height, size.width, size.height),
            candidate,
        )
    }
}

/// The full-window header. The left column holds the native window buttons
/// and title over the mode tabs, which sit on the header's bottom edge; the
/// transport circle takes the right end at full height; everything between is
/// a row of fat controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Header {
    pub band: Rect,
    pub strip: Rect,
}
impl Header {
    pub const STRIP: f64 = CONTROL + 4.0;
    pub fn new(width: f64, chrome: WindowChrome) -> Self {
        Self {
            band: Rect::new(0.0, 0.0, width, chrome.height),
            strip: Rect::new(0.0, chrome.height, width, chrome.height + Self::STRIP),
        }
    }
    pub fn bottom(&self) -> f64 {
        self.strip.y1
    }
    /// Mode tab spanning `x0..x1`, standing on the header's bottom edge.
    pub fn tab(&self, x0: f64, x1: f64) -> Rect {
        Rect::new(x0, self.bottom() - CONTROL, x1, self.bottom())
    }
    /// Square bounds of the transport circle at the right end.
    pub fn transport(&self) -> Rect {
        let x1 = self.band.x1 - INSET;
        Rect::new(x1 - self.fat_height(), GAP, x1, self.bottom() - GAP)
    }
    /// Fat control spanning `x0..x1` at the full header height minus margins.
    pub fn fat(&self, x0: f64, x1: f64) -> Rect {
        Rect::new(x0, GAP, x1, self.bottom() - GAP)
    }
    /// Regular control at `x`, aligned to the top margin of the fat row.
    pub fn row(&self, x: f64, width: f64) -> Rect {
        Rect::new(x, GAP, x + width, GAP + CONTROL)
    }
    /// Control ending at `x1` in the upper half of the fat row.
    pub fn upper(&self, x1: f64, width: f64) -> Rect {
        Rect::new(x1 - width, GAP, x1, GAP + self.half_height())
    }
    /// Control ending at `x1` in the lower half of the fat row, dropped
    /// halfway into the bottom margin.
    pub fn lower(&self, x1: f64, width: f64) -> Rect {
        let y1 = self.bottom() - GAP * 0.5;
        Rect::new(x1 - width, y1 - self.half_height(), x1, y1)
    }
    fn half_height(&self) -> f64 {
        (self.fat_height() - 2.0) * 0.5
    }
    fn fat_height(&self) -> f64 {
        self.bottom() - 2.0 * GAP
    }
}

/// Shared page edges and vertical bands for the room workspace. Drawing receives
/// content bounds: individual sections must not add another page gutter.
pub struct RoomLayout {
    pub header: Header,
    pub body: Rect,
    pub actions: Rect,
    pub status: Rect,
}
impl RoomLayout {
    pub fn new(size: Size, chrome: WindowChrome) -> Self {
        let status = Rect::new(0.0, size.height - 26.0, size.width, size.height);
        let header = Header::new(size.width, chrome);
        let actions = Rect::new(
            INSET,
            status.y0 - CONTROL - 2.0 * INSET,
            size.width - INSET,
            status.y0,
        );
        let body = Rect::new(
            INSET,
            header.bottom() + INSET,
            size.width - INSET,
            actions.y0 - INSET,
        );
        Self {
            header,
            body,
            actions,
            status,
        }
    }
    pub fn columns(bounds: Rect) -> [Rect; 2] {
        let half = (bounds.width() - INSET) * 0.5;
        [
            Rect::new(bounds.x0, bounds.y0, bounds.x0 + half, bounds.y1),
            Rect::new(bounds.x1 - half, bounds.y0, bounds.x1, bounds.y1),
        ]
    }
}

/// Drawing and input share geometry in points. Native window chrome is
/// measured by AppKit; content grows without changing control or font sizes.
pub struct Layout {
    pub header: Header,
    pub sidebar: Rect,
    pub listing: Rect,
    pub main: Rect,
    pub toast: Option<Rect>,
    pub graph: Rect,
    pub plot: Rect,
    pub legend: Rect,
    pub listening: Rect,
    pub details: Rect,
    pub details_header: Rect,
    pub status: Rect,
}
impl Layout {
    pub const RESULT_HEIGHT: f64 = LIST_ROW_HEIGHT;
    const ROUTE_ROW_TOP: f64 = 60.0;
    pub const ROUTE_SEPARATOR_TOP: f64 = Self::ROUTE_ROW_TOP + CONTROL + 20.0;
    pub const PROFILE_HEADER_TOP: f64 = Self::ROUTE_SEPARATOR_TOP + 8.0;
    pub const MODES_TOP: f64 = Self::PROFILE_HEADER_TOP + ROW;
    pub const SOURCE_TOP: f64 = Self::MODES_TOP + ROW;
    const GRAPH_TOP: f64 = Self::SOURCE_TOP + ROW;
    const LISTING_TOP: f64 = 156.0;
    pub fn new(size: Size, expanded: bool, candidate: bool, chrome: WindowChrome) -> Self {
        let status = Rect::new(0.0, size.height - 26.0, size.width, size.height);
        let header = Header::new(size.width, chrome);
        let top = header.bottom();
        let main = Rect::new(SIDEBAR + RULE, top, size.width, status.y0);
        let sidebar = Rect::new(0.0, top, SIDEBAR, status.y0);
        let listing = Rect::new(0.0, top + Self::LISTING_TOP, SIDEBAR, status.y0 - 36.0);
        let details_height = if expanded {
            SECTION_TOP + CONTROL + SECTION_GAP + 3.0 * CONTROL + SECTION_BOTTOM
        } else {
            4.0 + CONTROL + 4.0
        };
        let details = Rect::new(main.x0, status.y0 - details_height, size.width, status.y0);
        let details_header = Rect::new(
            main.x0,
            details.y0 + if expanded { SECTION_TOP } else { 4.0 },
            size.width,
            details.y0 + if expanded { SECTION_TOP } else { 4.0 } + CONTROL,
        );
        let listening_height = SECTION_TOP + CONTROL + SECTION_GAP + 3.0 * ROW + SECTION_BOTTOM;
        let listening = Rect::new(
            main.x0,
            details.y0 - listening_height,
            size.width,
            details.y0,
        );
        let graph = Rect::new(
            main.x0 + INSET,
            top + Self::GRAPH_TOP,
            size.width - INSET,
            listening.y0 - INSET,
        );
        let GraphLayout {
            plot,
            legend,
            toast,
            ..
        } = GraphLayout::new(graph, candidate);
        Self {
            header,
            sidebar,
            listing,
            main,
            toast,
            graph,
            plot,
            legend,
            listening,
            details,
            details_header,
            status,
        }
    }
    pub fn graph_layout(&self) -> GraphLayout {
        GraphLayout {
            graph: self.graph,
            plot: self.plot,
            legend: self.legend,
            toast: self.toast,
        }
    }
    pub fn listening_header(&self) -> Rect {
        self.section(self.listening.y0 + SECTION_TOP)
    }
    pub fn listening_row(&self, row: usize) -> Rect {
        let y = self.listening_header().y1 + SECTION_GAP + row as f64 * ROW;
        Rect::new(self.main.x0 + INSET, y, self.main.x1 - INSET, y + ROW)
    }
    pub fn detail_row(&self, row: usize) -> Rect {
        let y = self.details_header.y1 + SECTION_GAP + row as f64 * CONTROL;
        Rect::new(self.main.x0 + INSET, y, self.main.x1 - INSET, y + CONTROL)
    }
    pub fn volume(&self) -> Rect {
        self.listening_bar(0)
    }
    pub fn headroom(&self) -> Rect {
        self.listening_bar(2)
    }
    fn listening_bar(&self, row: usize) -> Rect {
        let row = self.listening_row(row);
        Rect::new(row.x0, row.y0 + 3.0, row.x1, row.y0 + 3.0 + CONTROL)
    }
    pub fn section(&self, y: f64) -> Rect {
        Rect::new(self.main.x0, y, self.main.x1, y + CONTROL)
    }
    pub fn row(&self, y: f64) -> Rect {
        Rect::new(self.main.x0 + INSET, y, self.main.x1 - INSET, y + CONTROL)
    }
    pub fn route_columns(&self) -> [Rect; 2] {
        let row = self.row(self.header.bottom() + Self::ROUTE_ROW_TOP);
        let half = (row.width() - 40.0) * 0.5;
        [
            Rect::new(row.x0, row.y0, row.x0 + half, row.y1),
            Rect::new(row.x1 - half, row.y0, row.x1, row.y1),
        ]
    }
    pub fn split_route(column: Rect) -> (Rect, Rect) {
        let pair = Rect::new(column.x1 - 62.0, column.y0, column.x1, column.y1);
        let device = Rect::new(column.x0, column.y0, pair.x0 - GAP, column.y1);
        (device, pair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_padding_and_controls_follow_native_chrome_without_scaling() {
        for size in [
            Size::new(880.0, MIN_HEIGHT),
            Size::new(1100.0, 860.0),
            Size::new(1440.0, 1000.0),
        ] {
            for expanded in [true, false] {
                for candidate in [true, false] {
                    for height in [28.0, 32.0, 44.0] {
                        let chrome = WindowChrome {
                            height,
                            controls_end: 81.0,
                        };
                        let layout = Layout::new(size, expanded, candidate, chrome);
                        let without_candidate = Layout::new(size, expanded, false, chrome);
                        assert_eq!(layout.graph, without_candidate.graph);
                        assert_eq!(layout.plot, without_candidate.plot);
                        if let Some(toast) = layout.toast {
                            assert_eq!(toast.intersect(layout.graph), toast);
                            assert!(toast.y1 < layout.legend.y0);
                        }
                        assert_eq!(layout.sidebar.y0, height + Header::STRIP);
                        assert_eq!(layout.sidebar.width(), 282.0);
                        assert_eq!(layout.volume().height(), 24.0);
                        assert_eq!(layout.main.x1 - layout.row(100.0).x1, 16.0);
                        assert_eq!(layout.graph.y1 - layout.legend.y1, 16.0);
                        assert!(layout.plot.width() > 480.0 && layout.plot.height() >= 32.0);
                        if expanded {
                            assert_eq!(layout.details_header.y0 - layout.details.y0, 12.0);
                            assert_eq!(layout.detail_row(0).y0 - layout.details_header.y1, 7.0);
                            assert_eq!(layout.details.y1 - layout.detail_row(2).y1, 8.0);
                        } else {
                            assert_eq!(layout.details_header.y0 - layout.details.y0, 4.0);
                            assert_eq!(layout.details.y1 - layout.details_header.y1, 4.0);
                        }
                    }
                }
            }
        }
    }
}
