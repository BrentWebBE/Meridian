//! Appearance: pick a mood, or let it follow the time of day.

use gtk::prelude::*;
use meridian_common::{
    config::Config,
    shell,
    theme::{BRAND, Mood},
};

use crate::{
    preview::{self, Overlay},
    widgets::{self, Loading},
};

pub fn page() -> gtk::Box {
    let page = widgets::page(
        "Appearance",
        &format!("Choose how {BRAND} looks, or let it follow the day."),
    );
    let config = Config::load().unwrap_or_default();
    let loading = Loading::default();

    let follow = gtk::Switch::new();
    let cards = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(14)
        .homogeneous(true)
        .build();

    let titles = [(Mood::Morning, "Morning"), (Mood::Dusk, "Dusk"), (Mood::Night, "Night")];
    let mut buttons: Vec<(Mood, gtk::ToggleButton)> = Vec::new();
    for (mood, title) in titles {
        let group = buttons.first().map(|(_, first)| first.clone());
        let card = widgets::card(&preview::landscape(Some(mood), Overlay::None), title, "", group.as_ref());

        // Choosing a mood pins it: the clock no longer decides.
        card.connect_toggled({
            let loading = loading.clone();
            let follow = follow.clone();
            move |card| {
                if card.is_active() && !loading.active() {
                    widgets::save("appearance", "theme", mood.name());
                    widgets::save("appearance", "follow-time-of-day", false);
                    loading.during(|| follow.set_active(false));
                }
            }
        });
        cards.append(&card);
        buttons.push((mood, card));
    }
    page.append(&cards);

    follow.connect_active_notify({
        let loading = loading.clone();
        move |follow| {
            if !loading.active() {
                widgets::save("appearance", "follow-time-of-day", follow.is_active());
            }
        }
    });
    page.append(&widgets::row(
        "Follow the time of day",
        Some("Morning, dusk and night switch automatically."),
        &follow,
    ));

    // Show the current state without it counting as a change.
    let current = config.appearance.current_mood(shell::current_hour());
    loading.during(|| {
        follow.set_active(config.appearance.follow_time_of_day);
        for (mood, card) in &buttons {
            card.set_active(*mood == current);
        }
    });

    page
}
