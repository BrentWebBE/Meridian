//! Building blocks shared by the settings pages.

use std::{cell::Cell, rc::Rc};

use gtk::prelude::*;
use meridian_common::config::{self, SettingValue};

/// Save one setting. The shell programs and the compositor notice the file
/// change on their own (inotify), so there is nothing else to do.
pub fn save(section: &str, key: &str, value: impl Into<SettingValue>) {
    if let Err(err) = config::write_setting(Some(section), key, value) {
        eprintln!("meridian-settings: {err}");
    }
}

/// True while a page sets its controls from the config. Handlers check it
/// so that showing the current value does not count as the user changing it.
#[derive(Clone, Default)]
pub struct Loading(Rc<Cell<bool>>);

impl Loading {
    pub fn active(&self) -> bool {
        self.0.get()
    }

    /// Run `f` with the flag set.
    pub fn during(&self, f: impl FnOnce()) {
        self.0.set(true);
        f();
        self.0.set(false);
    }
}

/// A page: heading, explanation, then whatever the caller adds.
pub fn page(title: &str, text: &str) -> gtk::Box {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["page"])
        .build();
    page.append(&label(title, "heading"));
    page.append(&label(text, "subheading"));
    page
}

pub fn label(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes([class])
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .build()
}

/// A choice card. Cards given the same `group` behave like radio buttons.
pub fn card(
    preview: &impl IsA<gtk::Widget>,
    title: &str,
    text: &str,
    group: Option<&gtk::ToggleButton>,
) -> gtk::ToggleButton {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(preview);
    content.append(&label(title, "card-title"));
    content.append(&label(text, "card-text"));

    let card = gtk::ToggleButton::builder().child(&content).css_classes(["card"]).build();
    card.set_group(group);
    card
}

/// A row with a title (and optional explanation) on the left and a control on the right.
pub fn row(title: &str, text: Option<&str>, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let texts = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    texts.append(&label(title, "row-title"));
    if let Some(text) = text {
        texts.append(&label(text, "row-text"));
    }
    control.set_valign(gtk::Align::Center);

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(16)
        .css_classes(["row"])
        .build();
    row.append(&texts);
    row.append(control);
    row
}
