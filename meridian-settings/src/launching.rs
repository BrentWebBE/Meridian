//! Launching apps: dock, auto-hide dock, or Find only.

use gtk::prelude::*;
use meridian_common::config::{Config, DockMode, DockPosition};

use crate::{
    preview::{self, Overlay},
    widgets::{self, Loading},
};

pub fn page() -> gtk::Box {
    let page = widgets::page(
        "Launching apps",
        "Use a dock, or leave the screen clear and press Super to find anything.",
    );
    let config = Config::load().unwrap_or_default();
    let loading = Loading::default();

    // ---- the three modes --------------------------------------------------
    // (value in the config file, mode, overlay on the preview, title, text)
    let modes = [
        ("dock", DockMode::Dock, Overlay::Dock, "Dock", "Always visible at the bottom."),
        ("auto-hide", DockMode::AutoHide, Overlay::Handle, "Auto-hide", "Slides up at the screen edge."),
        ("off", DockMode::Off, Overlay::Pill, "Find only", "No dock. Press Super."),
    ];
    let cards = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(14)
        .homogeneous(true)
        .build();
    let mut first: Option<gtk::ToggleButton> = None;
    let mut mode_cards = Vec::new();
    for (value, mode, overlay, title, text) in modes {
        let card = widgets::card(&preview::landscape(None, overlay), title, text, first.as_ref());
        card.connect_toggled({
            let loading = loading.clone();
            move |card| {
                if card.is_active() && !loading.active() {
                    widgets::save("dock", "mode", value);
                }
            }
        });
        cards.append(&card);
        first.get_or_insert_with(|| card.clone());
        mode_cards.push((mode, card));
    }
    page.append(&cards);

    // ---- dock position ----------------------------------------------------
    let positions = [
        ("bottom", DockPosition::Bottom, "Bottom"),
        ("left", DockPosition::Left, "Left"),
        ("right", DockPosition::Right, "Right"),
    ];
    let segmented = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["segmented"])
        .build();
    let mut first: Option<gtk::ToggleButton> = None;
    let mut position_buttons = Vec::new();
    for (value, position, title) in positions {
        let button = gtk::ToggleButton::with_label(title);
        button.set_group(first.as_ref());
        button.connect_toggled({
            let loading = loading.clone();
            move |button| {
                if button.is_active() && !loading.active() {
                    widgets::save("dock", "position", value);
                }
            }
        });
        segmented.append(&button);
        first.get_or_insert_with(|| button.clone());
        position_buttons.push((position, button));
    }
    page.append(&widgets::row("Dock position", None, &segmented));

    // ---- running dots -----------------------------------------------------
    let dots = gtk::Switch::new();
    dots.connect_active_notify({
        let loading = loading.clone();
        move |dots| {
            if !loading.active() {
                widgets::save("dock", "running-dots", dots.is_active());
            }
        }
    });
    page.append(&widgets::row("Show running app dots", None, &dots));

    // ---- Find shortcut (shown, not yet changeable) --------------------------
    let shortcut = gtk::Label::builder().label("Super").css_classes(["chip"]).build();
    page.append(&widgets::row(
        "Find shortcut",
        Some("Tap the Super key. Mod+Space works too."),
        &shortcut,
    ));

    // Show the current state without it counting as a change.
    loading.during(|| {
        for (mode, card) in &mode_cards {
            card.set_active(*mode == config.dock.mode);
        }
        for (position, button) in &position_buttons {
            button.set_active(*position == config.dock.position);
        }
        dots.set_active(config.dock.running_dots);
    });

    page
}
