//! Spaces (virtual desktops) and the overview.
//!
//! A space is a set of windows shown together. Only the active space's
//! windows are mapped in `Meridian::space`; the others are parked, alive but
//! not drawn. (Smithay's `Space` type is the 2D plane; our "spaces" are the
//! user-facing virtual desktops. The names collide, the concepts do not.)
//!
//! The overview shows the active space's windows as live thumbnails, with a
//! strip of all spaces above them.

use std::cell::Cell;

use smithay::{
    desktop::Window,
    utils::{Logical, Point},
};

use crate::{
    layout::{self, Rect},
    state::Meridian,
};

pub const SPACE_COUNT: usize = 4;

// Overview geometry, in logical pixels.
const SEARCH_HEIGHT: i32 = 84; // room for the launcher's search field
const TILE_HEIGHT: i32 = 84;
const TILE_GAP: i32 = 16;
const MARGIN: i32 = 48;

/// Where a window lives. Stored in the window's user data.
/// `Cell` allows changing a value through a shared reference; fine here
/// because everything runs on one thread.
struct Placement {
    space: Cell<usize>,
    /// Position to restore when its space becomes active again.
    parked_at: Cell<Option<Point<i32, Logical>>>,
}

/// What the overview looks like right now. Used for drawing and for clicks,
/// so both always agree.
pub struct OverviewScene {
    /// Windows of the active space with their thumbnail rectangles.
    pub thumbnails: Vec<(Window, Rect)>,
    pub spaces: Vec<SpaceTile>,
}

pub struct SpaceTile {
    pub rect: Rect,
    pub active: bool,
    /// The space's windows, shrunk to fit the tile.
    pub windows: Vec<(Window, Rect)>,
}

impl Meridian {
    // ---- spaces -----------------------------------------------------------

    fn placement<'a>(&self, window: &'a Window) -> &'a Placement {
        window.user_data().get_or_insert(|| Placement {
            space: Cell::new(self.active_space),
            parked_at: Cell::new(None),
        })
    }

    /// The space a window belongs to. New windows join the active space.
    pub fn window_space(&self, window: &Window) -> usize {
        self.placement(window).space.get()
    }

    /// The windows of one space, in the order they were opened.
    pub fn space_windows(&self, space: usize) -> Vec<Window> {
        self.windows
            .iter()
            .filter(|window| self.window_space(window) == space)
            .cloned()
            .collect()
    }

    /// Take a window off the screen, remembering where it was.
    fn park(&mut self, window: &Window) {
        let location = self.space.element_location(window);
        self.placement(window).parked_at.set(location);
        self.space.unmap_elem(window);
    }

    pub fn switch_space(&mut self, target: usize) {
        if target >= SPACE_COUNT || target == self.active_space {
            return;
        }

        for window in self.space_windows(self.active_space) {
            self.park(&window);
        }
        self.active_space = target;
        let origin = self.usable_area().map(|area| area.loc).unwrap_or_default();
        for window in self.space_windows(target) {
            let location = self
                .placement(&window)
                .parked_at
                .get()
                .unwrap_or((origin.x + 48, origin.y + 48).into());
            self.space.map_element(window, location, false);
        }

        self.apply_layout();
        // Focus the window that was on top, or nothing on an empty space.
        let top = self.space.elements().next_back().cloned();
        self.focus_window(top.as_ref());
        self.ipc_broadcast();
    }

    /// Send the focused window to another space; it disappears from this one.
    pub fn move_focused_to_space(&mut self, target: usize) {
        let Some(window) = self.focused_window() else { return };
        if target >= SPACE_COUNT || target == self.window_space(&window) {
            return;
        }

        self.park(&window);
        self.placement(&window).space.set(target);
        self.apply_layout();
        let top = self.space.elements().next_back().cloned();
        self.focus_window(top.as_ref());
        self.ipc_broadcast();
    }

    /// Make `window` visible (switching space if needed) and focus it.
    pub fn show_window(&mut self, window: &Window) {
        self.switch_space(self.window_space(window));
        self.focus_window(Some(window));
    }

    // ---- overview ---------------------------------------------------------

    pub fn set_overview(&mut self, open: bool) {
        if self.overview != open {
            self.overview = open;
            // The launcher listens for this and shows its search field.
            self.ipc_broadcast();
        }
    }

    /// Compute the overview's layout, or `None` when it is closed.
    pub fn overview_scene(&self) -> Option<OverviewScene> {
        if !self.overview {
            return None;
        }
        let area = self.usable_area()?;
        let output = self.output_area()?;

        // Strip of space tiles, centered under the search field. Each tile
        // has the proportions of the screen.
        let tile_width = TILE_HEIGHT * output.size.w / output.size.h.max(1);
        let strip_width = SPACE_COUNT as i32 * tile_width + (SPACE_COUNT as i32 - 1) * TILE_GAP;
        let strip_x = area.loc.x + (area.size.w - strip_width) / 2;
        let strip_y = area.loc.y + SEARCH_HEIGHT;
        let shrink = tile_width as f64 / output.size.w as f64;

        let spaces = (0..SPACE_COUNT)
            .map(|index| {
                let rect = Rect::new(
                    (strip_x + index as i32 * (tile_width + TILE_GAP), strip_y).into(),
                    (tile_width, TILE_HEIGHT).into(),
                );
                let windows = self
                    .space_windows(index)
                    .into_iter()
                    .filter_map(|window| {
                        // Where the window is (or was, if its space is parked).
                        let location = self
                            .space
                            .element_location(&window)
                            .or(self.placement(&window).parked_at.get())?;
                        let size = window.geometry().size;
                        let mini = Rect::new(
                            (
                                rect.loc.x + ((location.x - output.loc.x) as f64 * shrink) as i32,
                                rect.loc.y + ((location.y - output.loc.y) as f64 * shrink) as i32,
                            )
                                .into(),
                            (
                                ((size.w as f64 * shrink) as i32).max(2),
                                ((size.h as f64 * shrink) as i32).max(2),
                            )
                                .into(),
                        );
                        // Keep miniatures inside their tile.
                        mini.intersection(rect).map(|mini| (window, mini))
                    })
                    .collect();
                SpaceTile {
                    rect,
                    active: index == self.active_space,
                    windows,
                }
            })
            .collect();

        // Thumbnails fill what is left below the strip.
        let top = strip_y + TILE_HEIGHT + MARGIN / 2;
        let grid_area = Rect::new(
            (area.loc.x + MARGIN, top).into(),
            (
                (area.size.w - 2 * MARGIN).max(1),
                (area.loc.y + area.size.h - top - MARGIN / 2).max(1),
            )
                .into(),
        );
        let windows = self.space_windows(self.active_space);
        let sizes: Vec<_> = windows.iter().map(|window| window.geometry().size).collect();
        let thumbnails = windows.into_iter().zip(layout::grid(grid_area, &sizes, MARGIN / 2)).collect();

        Some(OverviewScene { thumbnails, spaces })
    }

    /// A click while the overview is open.
    pub fn overview_click(&mut self, pos: Point<f64, Logical>) {
        let Some(scene) = self.overview_scene() else { return };
        let point = pos.to_i32_round();

        if let Some((window, _)) = scene.thumbnails.iter().find(|(_, rect)| rect.contains(point)) {
            // Pick a window: leave the overview with it focused.
            self.set_overview(false);
            self.focus_window(Some(window));
        } else if let Some(index) = scene.spaces.iter().position(|tile| tile.rect.contains(point)) {
            // Pick a space: switch, but stay in the overview.
            self.switch_space(index);
        } else {
            self.set_overview(false);
        }
    }
}
