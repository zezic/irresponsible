//! Deadline sequence and redraw permits, adapted from Cordex and VU.
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use winit::dpi::PhysicalSize;
const BACKGROUND_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameRate {
    Fps15,
    Fps30,
    Fps48,
    Fps55,
    #[default]
    Fps60,
    Fps72,
    Fps90,
    Fps120,
    Fps180,
    Fps240,
    VSync,
}
impl FrameRate {
    pub const ALL: [Self; 11] = [
        Self::Fps15,
        Self::Fps30,
        Self::Fps48,
        Self::Fps55,
        Self::Fps60,
        Self::Fps72,
        Self::Fps90,
        Self::Fps120,
        Self::Fps180,
        Self::Fps240,
        Self::VSync,
    ];
    pub fn fps(self) -> Option<u64> {
        Some(match self {
            Self::Fps15 => 15,
            Self::Fps30 => 30,
            Self::Fps48 => 48,
            Self::Fps55 => 55,
            Self::Fps60 => 60,
            Self::Fps72 => 72,
            Self::Fps90 => 90,
            Self::Fps120 => 120,
            Self::Fps180 => 180,
            Self::Fps240 => 240,
            Self::VSync => return None,
        })
    }
    pub fn interval(self) -> Option<Duration> {
        self.fps()
            .map(|fps| Duration::from_nanos(1_000_000_000_u64.div_ceil(fps)))
    }
    pub fn label(self) -> String {
        self.fps()
            .map_or_else(|| "VSync".into(), |fps| format!("{fps} FPS"))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceArea {
    Empty,
    NonEmpty,
}

impl From<PhysicalSize<u32>> for SurfaceArea {
    fn from(size: PhysicalSize<u32>) -> Self {
        if size.width == 0 || size.height == 0 {
            Self::Empty
        } else {
            Self::NonEmpty
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occlusion {
    Visible,
    Occluded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Visible,
    Background,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedrawPermit {
    Idle,
    Requested,
}

/// Retains the deadline phase and skips expired slots without catch-up bursts.
pub struct FrameSchedule {
    area: SurfaceArea,
    occlusion: Occlusion,
    retry_at: Option<Instant>,
    next_due: Instant,
    permit: RedrawPermit,
}

impl FrameSchedule {
    pub fn new(now: Instant) -> Self {
        Self {
            area: SurfaceArea::Empty,
            occlusion: Occlusion::Visible,
            retry_at: None,
            next_due: now,
            permit: RedrawPermit::Idle,
        }
    }

    pub fn limit_changed(&mut self, now: Instant) {
        self.next_due = now;
    }

    pub fn kind(&self, now: Instant) -> FrameKind {
        let retrying = self.retry_at.is_some_and(|at| now < at);
        match (self.area, self.occlusion, retrying) {
            (SurfaceArea::NonEmpty, Occlusion::Visible, false) => FrameKind::Visible,
            _ => FrameKind::Background,
        }
    }

    pub fn is_due(&self, now: Instant) -> bool {
        now >= self.next_due
    }

    pub fn deadline(&self, now: Instant) -> Instant {
        self.retry_at
            .filter(|retry| *retry > now)
            .map_or(self.next_due, |retry| self.next_due.min(retry))
    }

    pub fn resized(&mut self, area: SurfaceArea, now: Instant) {
        self.area = area;
        self.wake_if_drawable(now);
    }

    pub fn occlusion_changed(&mut self, occlusion: Occlusion, now: Instant) {
        self.occlusion = occlusion;
        self.wake_if_drawable(now);
    }

    pub fn request_visible_frame(&mut self, now: Instant, limit: FrameRate) {
        assert!(
            self.kind(now) == FrameKind::Visible,
            "only drawable windows request visible frames"
        );
        assert!(
            self.is_due(now),
            "timer frame requested before its deadline"
        );
        self.retry_at = None;
        self.next_due = limit.interval().map_or(now, |interval| {
            let remainder = now.duration_since(self.next_due).as_nanos() % interval.as_nanos();
            now + interval - Duration::from_nanos(remainder as u64)
        });
        self.permit = RedrawPermit::Requested;
    }

    pub fn maintenance_finished(&mut self, now: Instant) {
        self.next_due = now + BACKGROUND_INTERVAL;
    }

    pub fn surface_unavailable(&mut self, now: Instant) {
        let retry_at = now + BACKGROUND_INTERVAL;
        self.retry_at = Some(retry_at);
        self.next_due = retry_at;
        self.permit = RedrawPermit::Idle;
    }

    pub fn take_redraw_permit(&mut self) -> RedrawPermit {
        std::mem::replace(&mut self.permit, RedrawPermit::Idle)
    }

    pub fn redraw_is_pending(&self) -> bool {
        self.permit == RedrawPermit::Requested
    }

    fn wake_if_drawable(&mut self, now: Instant) {
        if self.area == SurfaceArea::NonEmpty && self.occlusion == Occlusion::Visible {
            self.retry_at = None;
        } else {
            self.permit = RedrawPermit::Idle;
        }
        self.next_due = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_wakes_preserve_every_capped_cadence_without_catch_up() {
        for limit in FrameRate::ALL {
            let Some(interval) = limit.interval() else {
                continue;
            };
            let start = Instant::now();
            let mut schedule = FrameSchedule::new(start);
            schedule.resized(SurfaceArea::NonEmpty, start);
            schedule.request_visible_frame(start, limit);
            for frame in 1..1000 {
                let wake = start + interval * frame + Duration::from_micros(800);
                schedule.request_visible_frame(wake, limit);
                assert_eq!(schedule.deadline(wake), start + interval * (frame + 1));
                assert!(!schedule.is_due(wake));
            }
            let late = start + interval * 2000 + Duration::from_micros(800);
            schedule.request_visible_frame(late, limit);
            assert_eq!(schedule.deadline(late), start + interval * 2001);
        }
    }
    #[test]
    fn redraw_permits_are_consumed_once_and_vsync_has_no_timer_cap() {
        let start = Instant::now();
        let mut schedule = FrameSchedule::new(start);
        schedule.resized(SurfaceArea::NonEmpty, start);
        assert_eq!(schedule.take_redraw_permit(), RedrawPermit::Idle);
        schedule.request_visible_frame(start, FrameRate::Fps60);
        assert!(schedule.redraw_is_pending());
        assert_eq!(schedule.take_redraw_permit(), RedrawPermit::Requested);
        assert_eq!(schedule.take_redraw_permit(), RedrawPermit::Idle);
        let changed = start + Duration::from_millis(2);
        schedule.limit_changed(changed);
        schedule.request_visible_frame(changed, FrameRate::VSync);
        assert_eq!(schedule.deadline(changed), changed);
        assert!(FrameRate::VSync.interval().is_none());
    }
    #[test]
    fn hidden_empty_and_failed_surfaces_back_off_and_resume_promptly() {
        let start = Instant::now();
        let mut schedule = FrameSchedule::new(start);
        schedule.resized(SurfaceArea::NonEmpty, start);
        schedule.request_visible_frame(start, FrameRate::VSync);
        schedule.occlusion_changed(Occlusion::Occluded, start);
        assert_eq!(schedule.take_redraw_permit(), RedrawPermit::Idle);
        assert_eq!(schedule.kind(start), FrameKind::Background);
        schedule.maintenance_finished(start);
        assert_eq!(schedule.deadline(start), start + BACKGROUND_INTERVAL);
        let restored = start + Duration::from_millis(10);
        schedule.occlusion_changed(Occlusion::Visible, restored);
        assert!(schedule.is_due(restored));
        schedule.surface_unavailable(restored);
        assert_eq!(schedule.kind(restored), FrameKind::Background);
        assert_eq!(schedule.deadline(restored), restored + BACKGROUND_INTERVAL);
        assert_eq!(
            schedule.kind(restored + BACKGROUND_INTERVAL),
            FrameKind::Visible
        );
        schedule.resized(SurfaceArea::Empty, restored);
        schedule.occlusion_changed(Occlusion::Visible, restored);
        assert_eq!(schedule.kind(restored), FrameKind::Background);
    }
}
