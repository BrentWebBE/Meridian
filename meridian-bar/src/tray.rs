//! The tray at the right of the bar: network, volume and the notification bell.
//! (The battery lives in main.rs; it only reads two small files.)
//!
//! Nothing here may block the UI: slow sources are asked asynchronously.

use std::{cell::Cell, rc::Rc};

use gtk::{gio, glib, prelude::*};

// ---- network ---------------------------------------------------------------

/// Keep the network icon current: Wi-Fi, wired or offline.
pub fn watch_network(image: &gtk::Image) {
    // GIO watches the kernel's network state and tells us when it changes.
    let monitor = gio::NetworkMonitor::default();
    update_network(image, monitor.is_network_available());
    monitor.connect_network_changed(glib::clone!(
        #[weak]
        image,
        move |_, available| update_network(&image, available)
    ));
    // The monitor is a process-wide singleton, so the handler stays alive.
}

fn update_network(image: &gtk::Image, available: bool) {
    let (icon, text) = match (available, wireless_is_up()) {
        (false, _) => ("network-offline-symbolic", "Offline"),
        (true, true) => ("network-wireless-signal-good-symbolic", "Wi-Fi connected"),
        (true, false) => ("network-wired-symbolic", "Wired network"),
    };
    image.set_icon_name(Some(icon));
    image.set_tooltip_text(Some(text));
}

/// True if a Wi-Fi interface is up. Wireless interfaces have a `wireless`
/// directory in sysfs.
fn wireless_is_up() -> bool {
    let Ok(interfaces) = std::fs::read_dir("/sys/class/net") else {
        return false;
    };
    interfaces.flatten().any(|entry| {
        let path = entry.path();
        path.join("wireless").exists()
            && std::fs::read_to_string(path.join("operstate")).is_ok_and(|state| state.trim() == "up")
    })
}

// ---- volume ----------------------------------------------------------------

/// Show the output volume, asking PipeWire's `wpctl` every few seconds.
/// Hidden if `wpctl` is not installed.
pub fn watch_volume(image: &gtk::Image) {
    let image = image.clone();
    // An async block on the GTK main loop: `await` waits without freezing the UI.
    glib::spawn_future_local(async move {
        loop {
            match read_volume().await {
                Some((level, muted)) => {
                    image.set_icon_name(Some(volume_icon(level, muted)));
                    let percent = (level * 100.0).round() as u32;
                    image.set_tooltip_text(Some(&if muted {
                        "Muted".to_string()
                    } else {
                        format!("Volume {percent}%")
                    }));
                    image.set_visible(true);
                }
                None => image.set_visible(false),
            }
            glib::timeout_future_seconds(3).await;
        }
    });
}

async fn read_volume() -> Option<(f64, bool)> {
    let process = gio::Subprocess::newv(
        &["wpctl".as_ref(), "get-volume".as_ref(), "@DEFAULT_AUDIO_SINK@".as_ref()],
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .ok()?;
    let (stdout, _) = process.communicate_utf8_future(None).await.ok()?;
    parse_volume(&stdout?)
}

/// Parse `wpctl` output such as `Volume: 0.45` or `Volume: 0.45 [MUTED]`.
fn parse_volume(output: &str) -> Option<(f64, bool)> {
    let mut words = output.split_whitespace();
    if words.next()? != "Volume:" {
        return None;
    }
    let level: f64 = words.next()?.parse().ok()?;
    Some((level, output.contains("[MUTED]")))
}

fn volume_icon(level: f64, muted: bool) -> &'static str {
    if muted || level <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if level < 0.34 {
        "audio-volume-low-symbolic"
    } else if level < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

// ---- notification bell -----------------------------------------------------

const NOTIFY_NAME: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/meridian/Notifications";
const NOTIFY_INTERFACE: &str = "org.meridian.Notifications";

/// The bell, with a dot while there are notifications you have not seen.
/// meridian-notify tells us the count over D-Bus; clicking marks them read.
pub fn bell() -> gtk::Button {
    let dot = gtk::Box::builder()
        .css_classes(["unread-dot"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .visible(false)
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&gtk::Image::from_icon_name("preferences-system-notifications-symbolic")));
    overlay.add_overlay(&dot);

    let button = gtk::Button::builder()
        .child(&overlay)
        .css_classes(["bell"])
        .valign(gtk::Align::Center)
        .tooltip_text("No new notifications")
        .build();

    // The connection arrives asynchronously; the click handler picks it up
    // from this shared slot once it is there.
    let connection: Rc<Cell<Option<gio::DBusConnection>>> = Rc::new(Cell::new(None));

    gio::bus_get(
        gio::BusType::Session,
        gio::Cancellable::NONE,
        glib::clone!(
            #[weak]
            button,
            #[weak]
            dot,
            #[strong]
            connection,
            move |result| {
                let Ok(bus) = result else { return };
                // Run the closure whenever meridian-notify emits `UnreadChanged`.
                #[allow(deprecated)] // the replacement API is newer than our minimum GLib
                let _subscription = bus.signal_subscribe(
                    None,
                    Some(NOTIFY_INTERFACE),
                    Some("UnreadChanged"),
                    Some(NOTIFY_PATH),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_, _, _, _, _, parameters| {
                        let unread = parameters.get::<(u32,)>().map_or(0, |(count,)| count);
                        dot.set_visible(unread > 0);
                        button.set_tooltip_text(Some(&match unread {
                            0 => "No new notifications".to_string(),
                            1 => "1 new notification".to_string(),
                            count => format!("{count} new notifications"),
                        }));
                    },
                );
                connection.set(Some(bus));
            }
        ),
    );

    button.connect_clicked(move |_| {
        // `Cell` hands the value out by moving it; put it back afterwards.
        let Some(bus) = connection.take() else { return };
        bus.call(
            Some(NOTIFY_NAME),
            NOTIFY_PATH,
            NOTIFY_INTERFACE,
            "MarkRead",
            None,
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
            |_| {}, // nothing to do with the answer
        );
        connection.set(Some(bus));
    });

    button
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpctl_output_is_parsed() {
        assert_eq!(parse_volume("Volume: 0.45\n"), Some((0.45, false)));
        assert_eq!(parse_volume("Volume: 1.00 [MUTED]\n"), Some((1.0, true)));
        assert_eq!(parse_volume("error"), None);
    }

    #[test]
    fn volume_icon_follows_level() {
        assert_eq!(volume_icon(0.9, false), "audio-volume-high-symbolic");
        assert_eq!(volume_icon(0.5, false), "audio-volume-medium-symbolic");
        assert_eq!(volume_icon(0.1, false), "audio-volume-low-symbolic");
        assert_eq!(volume_icon(0.9, true), "audio-volume-muted-symbolic");
    }
}
