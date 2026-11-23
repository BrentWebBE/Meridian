//! Window layouts.
//!
//! A layout is pure geometry: given the usable screen area and the number of
//! windows, where does each window go? It knows nothing about Wayland, which
//! keeps it easy to test and easy to add new layouts.

use meridian_common::config::{Config, LayoutKind};
use smithay::utils::{Logical, Rectangle, Size};

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

/// Arrange thumbnails of windows with the given sizes in a grid inside
/// `area`. Each window keeps its proportions and is never enlarged.
/// Used by the overview.
pub fn grid(area: Rect, sizes: &[Size<i32, Logical>], gap: i32) -> Vec<Rect> {
    let count = sizes.len() as i32;
    if count == 0 {
        return Vec::new();
    }
    // As square as possible: 1 -> 1x1, 2 -> 2x1, 5 -> 3x2, ...
    let columns = (count as f64).sqrt().ceil() as i32;
    let rows = (count + columns - 1) / columns;
    let cell_w = ((area.size.w - gap * (columns - 1)) / columns).max(1);
    let cell_h = ((area.size.h - gap * (rows - 1)) / rows).max(1);

    sizes
        .iter()
        .enumerate()
        .map(|(index, size)| {
            let (column, row) = (index as i32 % columns, index as i32 / columns);
            let scale = (cell_w as f64 / size.w.max(1) as f64)
                .min(cell_h as f64 / size.h.max(1) as f64)
                .min(1.0);
            let w = ((size.w as f64 * scale) as i32).max(1);
            let h = ((size.h as f64 * scale) as i32).max(1);
            // Center the thumbnail in its cell.
            let x = area.loc.x + column * (cell_w + gap) + (cell_w - w) / 2;
            let y = area.loc.y + row * (cell_h + gap) + (cell_h - h) / 2;
            Rect::new((x, y).into(), (w, h).into())
        })
        .collect()
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

    #[test]
    fn grid_keeps_proportions_and_fits() {
        let sizes = vec![Size::from((800, 600)); 5];
        let rects = grid(area(), &sizes, 20);
        assert_eq!(rects.len(), 5);
        for (i, a) in rects.iter().enumerate() {
            assert!(area().contains_rect(*a), "{a:?} outside the area");
            // 4:3 stays 4:3 (within rounding).
            assert!((a.size.w * 3 - a.size.h * 4).abs() <= 4, "{a:?} lost its proportions");
            for b in &rects[i + 1..] {
                assert!(!a.overlaps(*b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn grid_never_enlarges_small_windows() {
        let rects = grid(area(), &[Size::from((200, 100))], 20);
        assert_eq!(rects[0].size, Size::from((200, 100)));
        // Centered in the area.
        assert_eq!(rects[0].loc, (400, 250).into());
    }

    #[test]
    fn grid_of_nothing_is_empty() {
        assert!(grid(area(), &[], 20).is_empty());
    }
}
