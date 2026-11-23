//! Small drawn previews for the choice cards: the Terra landscape in a
//! mood's colors, optionally with a dock, a handle or a Find pill on it.

use std::f64::consts::PI;

use gtk::{cairo, prelude::*};
use meridian_common::{
    shell,
    theme::{Color, Mood, Themes},
};

#[derive(Clone, Copy)]
pub enum Overlay {
    None,
    Dock,
    Handle,
    Pill,
}

const WIDTH: i32 = 190;
const HEIGHT: i32 = 104;

fn set_color(cr: &cairo::Context, color: Color) {
    cr.set_source_rgba(color.r as f64, color.g as f64, color.b as f64, color.a as f64);
}

/// A rectangle with all four corners rounded.
fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    cr.new_path();
    cr.arc(x + w - radius, y + radius, radius, -PI / 2.0, 0.0);
    cr.arc(x + w - radius, y + h - radius, radius, 0.0, PI / 2.0);
    cr.arc(x + radius, y + h - radius, radius, PI / 2.0, PI);
    cr.arc(x + radius, y + radius, radius, PI, 1.5 * PI);
    cr.close_path();
}

/// The landscape in the given mood (`None` = whatever mood is active now).
pub fn landscape(mood: Option<Mood>, overlay: Overlay) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(WIDTH)
        .content_height(HEIGHT)
        .build();

    // GTK calls this whenever the widget needs painting.
    area.set_draw_func(move |_, cr, width, height| {
        let mood = mood.unwrap_or_else(|| Mood::from_name(&shell::current_mood()));
        let theme = Themes::load().get(mood);
        let (w, h) = (width as f64, height as f64);

        // Everything is clipped to a rounded rectangle.
        rounded_rect(cr, 0.0, 0.0, w, h, 10.0);
        cr.clip();

        shell::paint_landscape(cr, w, h, &theme);

        set_color(cr, theme.surface);
        match overlay {
            Overlay::None => {}
            Overlay::Dock => {
                rounded_rect(cr, w * 0.31, h - 30.0, w * 0.38, 18.0, 7.0);
                cr.fill().ok();
            }
            Overlay::Handle => {
                set_color(cr, theme.ink.with_alpha(0.6));
                rounded_rect(cr, w * 0.38, h - 9.0, w * 0.24, 3.0, 1.5);
                cr.fill().ok();
            }
            Overlay::Pill => {
                rounded_rect(cr, w * 0.27, h * 0.30, w * 0.46, 20.0, 10.0);
                cr.fill().ok();
            }
        }
    });
    area
}
