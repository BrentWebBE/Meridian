//! Window layouts.
//!
//! A layout is pure geometry: given the usable screen area and the number of
//! windows, where does each window go? It knows nothing about Wayland, which
//! keeps it easy to test and easy to add new layouts.

use meridian_common::config::{Config, LayoutKind};
use smithay::utils::{Logical, Rectangle};

/// A rectangle in logical (scale-independent) screen coordinates.
pub type Rect = Rectangle<i32, Logical>;

/// `trait` is Rust's word for an interface. The compositor stores the active
/// layout as `Box<dyn Layout>`: a "trait object", i.e. a heap pointer to
/// *some* type implementing `Layout`, chosen at runtime. That is what lets the
/// user switch layouts while Meridian is running.
pub trait Layout {
    fn kind(&self) -> LayoutKind;

    /// One rectangle per window, in window order.
    /// `None` means "I do not manage positions": windows stay where the user put them.
    fn arrange(&self, area: Rect, count: usize) -> Option<Vec<Rect>>;
}

pub fn from_kind(kind: LayoutKind, config: &Config) -> Box<dyn Layout> {
    match kind {
        LayoutKind::Floating => Box::new(Floating),
        LayoutKind::Tiled => Box::new(Tiled {
            gap: config.tiled.gap.max(0),
            master_ratio: config.tiled.master_ratio.clamp(0.1, 0.9),
        }),
    }
}

/// Classic desktop behavior: windows overlap and you move them yourself.
pub struct Floating;

impl Layout for Floating {
    fn kind(&self) -> LayoutKind {
        LayoutKind::Floating
    }

    fn arrange(&self, _area: Rect, _count: usize) -> Option<Vec<Rect>> {
        None
    }
}

/// "Master and stack": the first window takes the left part of the screen,
/// all others share the right part, stacked vertically.
pub struct Tiled {
    pub gap: i32,
    pub master_ratio: f64,
}

impl Layout for Tiled {
    fn kind(&self) -> LayoutKind {
        LayoutKind::Tiled
    }

    fn arrange(&self, area: Rect, count: usize) -> Option<Vec<Rect>> {
        let gap = self.gap;
        // Shrink by the gap on every side.
        let inner = Rect::new(
            (area.loc.x + gap, area.loc.y + gap).into(),
            ((area.size.w - 2 * gap).max(1), (area.size.h - 2 * gap).max(1)).into(),
        );

        if count == 0 {
            return Some(Vec::new());
        }
        if count == 1 {
            return Some(vec![inner]);
        }

        let master_width = ((inner.size.w - gap) as f64 * self.master_ratio).round() as i32;
        let stack_x = inner.loc.x + master_width + gap;
        let stack_width = (inner.size.w - master_width - gap).max(1);

        let stack_count = (count - 1) as i32;
        let stack_height = ((inner.size.h - gap * (stack_count - 1)) / stack_count).max(1);

        let mut rects = vec![Rect::new(inner.loc, (master_width, inner.size.h).into())];
        for i in 0..stack_count {
            let y = inner.loc.y + i * (stack_height + gap);
            // The last window takes the leftover pixels from integer division.
            let height = if i == stack_count - 1 {
                (inner.loc.y + inner.size.h - y).max(1)
            } else {
                stack_height
            };
            rects.push(Rect::new((stack_x, y).into(), (stack_width, height).into()));
        }
        Some(rects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new((0, 0).into(), (1000, 600).into())
    }

    fn tiled() -> Tiled {
        Tiled { gap: 10, master_ratio: 0.5 }
    }

    #[test]
    fn floating_leaves_windows_alone() {
        assert!(Floating.arrange(area(), 3).is_none());
    }

    #[test]
    fn single_window_fills_the_area_minus_gaps() {
        let rects = tiled().arrange(area(), 1).unwrap();
        assert_eq!(rects, vec![Rect::new((10, 10).into(), (980, 580).into())]);
    }

    #[test]
    fn master_left_and_stack_right() {
        let rects = tiled().arrange(area(), 3).unwrap();
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0], Rect::new((10, 10).into(), (485, 580).into()));
        assert_eq!(rects[1], Rect::new((505, 10).into(), (485, 285).into()));
        assert_eq!(rects[2], Rect::new((505, 305).into(), (485, 285).into()));
    }

    #[test]
    fn tiles_never_overlap_and_stay_inside() {
        for count in 1..8 {
            let rects = tiled().arrange(area(), count).unwrap();
            assert_eq!(rects.len(), count);
            for (i, a) in rects.iter().enumerate() {
                assert!(area().contains_rect(*a), "{a:?} outside the area");
                for b in &rects[i + 1..] {
                    assert!(!a.overlaps(*b), "{a:?} overlaps {b:?}");
                }
            }
        }
    }
}
