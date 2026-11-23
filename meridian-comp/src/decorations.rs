//! Window decorations drawn by the compositor ("server-side decorations").
//!
//! A client can draw its own title bar (client-side decorations, CSD) or
//! leave it to us. The xdg-decoration protocol is how the two sides agree:
//! we offer to draw, and the client may accept or insist on its own.
//!
//! For windows we decorate, a 42px title bar sits directly above the
//! client's content: title on the left, minimize, maximize and close on the
//! right. The bar is painted with cairo (shapes) and pango (text) into a
//! small image, which is cached until something about it changes.

use std::cell::{Cell, RefCell};

use meridian_common::theme::{Color, Theme, WINDOW_RADIUS};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{Kind, memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement}},
            gles::GlesRenderer,
        },
    },
    desktop::Window,
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode, shell::server::xdg_toplevel,
        },
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Physical, Point, Transform},
    wayland::{
        compositor::with_states,
        shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData, decoration::XdgDecorationHandler},
    },
};

use crate::{grabs::Edges, layout::Rect, state::Meridian};

/// Height of the title bar, in logical pixels.
pub const TITLE_HEIGHT: i32 = 42;
/// Each button's clickable square, and the gap from the right edge.
const BUTTON_SIZE: i32 = 28;
const BUTTON_MARGIN: i32 = 10;
/// How close to a window's edge a press starts a resize.
const RESIZE_BAND: i32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleButton {
    Minimize,
    Maximize,
    Close,
}

/// What a click on a window's frame landed on.
pub enum FrameHit {
    /// The title bar itself: drag to move.
    Titlebar(Window),
    Button(Window, TitleButton),
    /// The border: drag to resize.
    Edge(Window, Edges),
}

/// Whether we draw `window`'s title bar. Stored in the window's user data.
struct Decorated(Cell<bool>);

pub fn is_decorated(window: &Window) -> bool {
    window.user_data().get::<Decorated>().is_some_and(|flag| flag.0.get())
}

fn set_decorated(window: &Window, decorated: bool) {
    window
        .user_data()
        .get_or_insert(|| Decorated(Cell::new(false)))
        .0
        .set(decorated);
}

/// The whole window as the user sees it: the client's content plus our
/// title bar above it, if we draw one. `content` is the client's rectangle.
pub fn frame_rect(window: &Window, content: Rect) -> Rect {
    if is_decorated(window) {
        Rect::new(
            (content.loc.x, content.loc.y - TITLE_HEIGHT).into(),
            (content.size.w, content.size.h + TITLE_HEIGHT).into(),
        )
    } else {
        content
    }
}

/// The clickable squares of the three buttons, for a title bar occupying
/// `bar`. Used both to draw them and to find out which one was clicked.
pub fn button_rects(bar: Rect) -> [(TitleButton, Rect); 3] {
    let y = bar.loc.y + (bar.size.h - BUTTON_SIZE) / 2;
    let right = bar.loc.x + bar.size.w - BUTTON_MARGIN;
    let square = |slot: i32| Rect::new((right - slot * BUTTON_SIZE, y).into(), (BUTTON_SIZE, BUTTON_SIZE).into());
    [
        (TitleButton::Minimize, square(3)),
        (TitleButton::Maximize, square(2)),
        (TitleButton::Close, square(1)),
    ]
}

fn window_title(window: &Window) -> String {
    let Some(toplevel) = window.toplevel() else {
        return String::new();
    };
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok()?.title.clone())
            .unwrap_or_default()
    })
}

// ---- drawing ---------------------------------------------------------------

/// Everything the painted title bar depends on. If none of it changed, the
/// cached image is reused.
#[derive(PartialEq)]
struct TitlebarKey {
    title: String,
    width: i32,
    focused: bool,
    theme: Theme,
    scale: i32,
}

struct TitlebarCache(RefCell<Option<(TitlebarKey, MemoryRenderBuffer)>>);

/// The title bar of `window` as a render element, positioned at `location`
/// (the frame's top-left corner on the output).
pub fn titlebar_element(
    renderer: &mut GlesRenderer,
    window: &Window,
    location: Point<i32, Logical>,
    width: i32,
    focused: bool,
    theme: Theme,
    scale: f64,
) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
    let key = TitlebarKey {
        title: window_title(window),
        width,
        focused,
        theme,
        // Paint at the next whole scale so text stays sharp on HiDPI.
        scale: scale.ceil().max(1.0) as i32,
    };

    let cache = window
        .user_data()
        .get_or_insert(|| TitlebarCache(RefCell::new(None)));
    let mut cached = cache.0.borrow_mut();
    if cached.as_ref().is_none_or(|(cached_key, _)| *cached_key != key) {
        let buffer = paint_titlebar(&key)?;
        *cached = Some((key, buffer));
    }
    let (_, buffer) = cached.as_ref()?;

    let physical: Point<f64, Physical> = location.to_f64().to_physical(scale);
    MemoryRenderBufferRenderElement::from_buffer(renderer, physical, buffer, None, None, None, Kind::Unspecified).ok()
}

fn set_color(cr: &cairo::Context, color: Color) {
    cr.set_source_rgba(color.r as f64, color.g as f64, color.b as f64, color.a as f64);
}

/// Paint a title bar with cairo. Returns `None` if cairo refuses (for
/// example a zero-sized window).
fn paint_titlebar(key: &TitlebarKey) -> Option<MemoryRenderBuffer> {
    let (width, height) = (key.width, TITLE_HEIGHT);
    if width <= 0 {
        return None;
    }
    let (pixel_w, pixel_h) = (width * key.scale, height * key.scale);
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pixel_w, pixel_h).ok()?;

    {
        let cr = cairo::Context::new(&surface).ok()?;
        // From here on we draw in logical pixels.
        cr.scale(key.scale as f64, key.scale as f64);
        let theme = &key.theme;
        let (w, h) = (width as f64, height as f64);
        let radius = WINDOW_RADIUS as f64;

        // Background: rounded at the top, square where it meets the content.
        cr.new_path();
        cr.arc(radius, radius, radius, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.arc(w - radius, radius, radius, 1.5 * std::f64::consts::PI, 0.0);
        cr.line_to(w, h);
        cr.line_to(0.0, h);
        cr.close_path();
        set_color(&cr, theme.surface2);
        cr.fill().ok()?;

        // Hairline between title bar and content.
        set_color(&cr, theme.border());
        cr.rectangle(0.0, h - 1.0, w, 1.0);
        cr.fill().ok()?;

        let bar = Rect::new((0, 0).into(), (width, height).into());
        let buttons = button_rects(bar);
        let text_color = if key.focused { theme.ink } else { theme.muted };

        // Title, cut off with "..." before it reaches the buttons.
        let layout = pangocairo::functions::create_layout(&cr);
        let mut font = pango::FontDescription::from_string("DM Sans, sans-serif");
        font.set_weight(pango::Weight::Semibold);
        font.set_absolute_size(13.0 * pango::SCALE as f64);
        layout.set_font_description(Some(&font));
        layout.set_text(&key.title);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        let text_left = 18.0;
        let text_width = (buttons[0].1.loc.x as f64 - text_left - 12.0).max(0.0);
        layout.set_width((text_width * pango::SCALE as f64) as i32);
        let (_, text_height) = layout.pixel_size();
        set_color(&cr, text_color);
        cr.move_to(text_left, (h - text_height as f64) / 2.0);
        pangocairo::functions::show_layout(&cr, &layout);

        // Buttons.
        cr.set_line_width(1.6);
        cr.set_line_cap(cairo::LineCap::Round);
        for (button, rect) in buttons {
            let cx = rect.loc.x as f64 + rect.size.w as f64 / 2.0;
            let cy = rect.loc.y as f64 + rect.size.h as f64 / 2.0;
            match button {
                TitleButton::Minimize => {
                    set_color(&cr, text_color);
                    cr.move_to(cx - 5.0, cy + 4.0);
                    cr.line_to(cx + 5.0, cy + 4.0);
                    cr.stroke().ok()?;
                }
                TitleButton::Maximize => {
                    set_color(&cr, text_color);
                    cr.rectangle(cx - 4.5, cy - 4.5, 9.0, 9.0);
                    cr.stroke().ok()?;
                }
                TitleButton::Close => {
                    // An accent circle on the focused window, a quiet one otherwise.
                    let (fill, cross) = if key.focused {
                        (theme.accent, theme.on_accent)
                    } else {
                        (theme.border(), theme.muted)
                    };
                    set_color(&cr, fill);
                    cr.arc(cx, cy, 10.0, 0.0, 2.0 * std::f64::consts::PI);
                    cr.fill().ok()?;
                    set_color(&cr, cross);
                    cr.move_to(cx - 3.5, cy - 3.5);
                    cr.line_to(cx + 3.5, cy + 3.5);
                    cr.move_to(cx + 3.5, cy - 3.5);
                    cr.line_to(cx - 3.5, cy + 3.5);
                    cr.stroke().ok()?;
                }
            }
        }
    } // the cairo context is dropped here, releasing the surface

    surface.flush();
    let pixels = surface.data().ok()?;
    // Cairo's ARGB32 is premultiplied and, on little-endian machines, laid
    // out as B, G, R, A in memory: that is DRM's "ARGB8888".
    Some(MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Argb8888,
        (pixel_w, pixel_h),
        key.scale,
        Transform::Normal,
        None,
    ))
}

// ---- protocol --------------------------------------------------------------

impl XdgDecorationHandler for Meridian {
    /// A client is willing to discuss decorations: offer to draw them.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(&toplevel, Mode::ServerSide);
    }

    /// The client states a preference. We respect it either way.
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        self.set_decoration_mode(&toplevel, mode);
    }

    /// The client has no preference (any more): we draw.
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(&toplevel, Mode::ServerSide);
    }

    /// The client withdrew from the protocol: it decorates itself again.
    fn decoration_destroyed(&mut self, toplevel: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(toplevel.wl_surface()) {
            set_decorated(&window, false);
            self.apply_layout();
        }
    }
}

impl Meridian {
    fn set_decoration_mode(&mut self, toplevel: &ToplevelSurface, mode: Mode) {
        toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
        // Before the first configure the mode simply travels with it.
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        if let Some(window) = self.window_for_surface(toplevel.wl_surface()) {
            set_decorated(&window, mode == Mode::ServerSide);
            // A title bar changes how much room the content has when tiled.
            self.apply_layout();
        }
    }

    // ---- hit testing ------------------------------------------------------

    /// What part of a window frame is at `pos`, if any. Returns `None` over
    /// a window's content, over windows that decorate themselves, and over
    /// the empty desktop.
    pub fn frame_hit(&self, pos: Point<f64, Logical>) -> Option<FrameHit> {
        let point = pos.to_i32_floor();
        // Topmost window first: a frame hidden behind another window is not clickable.
        for window in self.space.elements().rev() {
            let Some(location) = self.space.element_location(window) else { continue };
            let content = Rect::new(location, window.geometry().size);

            if is_decorated(window) {
                let frame = frame_rect(window, content);
                // Borders resize, but only where windows float freely.
                if !self.layout_is_managed() {
                    let band = RESIZE_BAND;
                    let outer = Rect::new(
                        (frame.loc.x - band, frame.loc.y - band).into(),
                        (frame.size.w + 2 * band, frame.size.h + 2 * band).into(),
                    );
                    if outer.contains(point) {
                        let edges = Edges {
                            left: point.x < frame.loc.x + 2,
                            right: point.x >= frame.loc.x + frame.size.w - 2,
                            top: point.y < frame.loc.y + 3,
                            bottom: point.y >= frame.loc.y + frame.size.h - 2,
                        };
                        if edges != Edges::default() {
                            return Some(FrameHit::Edge(window.clone(), edges));
                        }
                    }
                }

                let bar = Rect::new(frame.loc, (frame.size.w, TITLE_HEIGHT).into());
                if bar.contains(point) {
                    let button = button_rects(bar).into_iter().find(|(_, rect)| rect.contains(point));
                    return Some(match button {
                        Some((button, _)) => FrameHit::Button(window.clone(), button),
                        None => FrameHit::Titlebar(window.clone()),
                    });
                }
            }

            // The content of this window covers whatever is below it.
            if content.contains(point) {
                return None;
            }
        }
        None
    }

    // ---- window states ----------------------------------------------------

    /// Fill the usable area, or go back to the size and place from before.
    pub fn toggle_maximize(&mut self, window: &Window) {
        if self.layout_is_managed() {
            return; // tiled windows already fill their share
        }
        let Some(toplevel) = window.toplevel() else { return };
        let placement = self.placement(window);

        match placement.restore.take() {
            Some(previous) => {
                toplevel.with_pending_state(|state| {
                    state.size = Some(previous.size);
                    state.states.unset(xdg_toplevel::State::Maximized);
                });
                toplevel.send_pending_configure();
                self.space.map_element(window.clone(), previous.loc, true);
            }
            None => {
                let (Some(location), Some(area)) = (self.space.element_location(window), self.usable_area()) else {
                    return;
                };
                placement.restore.set(Some(Rect::new(location, window.geometry().size)));
                let bar = if is_decorated(window) { TITLE_HEIGHT } else { 0 };
                toplevel.with_pending_state(|state| {
                    state.size = Some((area.size.w, area.size.h - bar).into());
                    state.states.set(xdg_toplevel::State::Maximized);
                });
                toplevel.send_pending_configure();
                self.space.map_element(window.clone(), (area.loc.x, area.loc.y + bar), true);
            }
        }
    }

    /// Hide a window until it is asked for again (dock, Find).
    pub fn minimize(&mut self, window: &Window) {
        self.placement(window).minimized.set(true);
        self.park(window);
        self.apply_layout();
        let top = self.space.elements().next_back().cloned();
        self.focus_window(top.as_ref());
        self.ipc_broadcast();
    }

    /// Requests from clients that draw their own buttons.
    pub fn client_maximize(&mut self, surface: &WlSurface, maximize: bool) {
        let Some(window) = self.window_for_surface(surface) else { return };
        let is_maximized = self.placement(&window).restore.get().is_some();
        if maximize != is_maximized {
            self.toggle_maximize(&window);
        } else if let Some(toplevel) = window.toplevel() {
            // The protocol wants an answer even when nothing changes.
            toplevel.send_configure();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_sit_at_the_right_in_order() {
        let bar = Rect::new((100, 50).into(), (400, TITLE_HEIGHT).into());
        let [(first, minimize), (second, maximize), (third, close)] = button_rects(bar);
        assert_eq!((first, second, third), (TitleButton::Minimize, TitleButton::Maximize, TitleButton::Close));
        // Left to right, touching, inside the bar.
        assert!(minimize.loc.x < maximize.loc.x && maximize.loc.x < close.loc.x);
        assert_eq!(minimize.loc.x + BUTTON_SIZE, maximize.loc.x);
        assert_eq!(close.loc.x + BUTTON_SIZE, 100 + 400 - BUTTON_MARGIN);
        for rect in [minimize, maximize, close] {
            assert!(bar.contains_rect(rect));
        }
    }

    #[test]
    fn title_bar_paints_at_any_scale() {
        for scale in [1, 2] {
            let key = TitlebarKey {
                title: "A window with quite a long title that must be cut off".into(),
                width: 300,
                focused: true,
                theme: meridian_common::theme::Themes::builtin().morning,
                scale,
            };
            assert!(paint_titlebar(&key).is_some());
        }
        let empty = TitlebarKey {
            title: String::new(),
            width: 0,
            focused: false,
            theme: meridian_common::theme::Themes::builtin().night,
            scale: 1,
        };
        assert!(paint_titlebar(&empty).is_none());
    }
}
