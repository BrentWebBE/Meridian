//! Meridian's lock screen.
//!
//! It asks the compositor to lock the session (ext-session-lock-v1), shows
//! the clock and a password field on every monitor, and asks it to unlock
//! once PAM accepts the password. The compositor enforces the lock; this
//! program only draws the dialog and checks the password.
//!
//! The password is read once from the field, handed to a worker thread for
//! the PAM check, and overwritten in memory afterwards. It is never logged.

mod auth;

use std::{cell::Cell, path::Path, rc::Rc};

use gtk::{gdk, glib, prelude::*};
use meridian_common::{
    shell,
    theme::{Mood, Themes},
};
use zeroize::Zeroizing;

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("org.meridian.Lock")
        .build();
    app.connect_activate(|app| {
        // A second start while locked changes nothing.
        if app.windows().is_empty() {
            lock(app);
        }
    });
    app.run()
}

/// Whether locking is safe: without our PAM rules every password would be
/// refused and the session could not be unlocked.
fn can_authenticate() -> bool {
    if Path::new(auth::SERVICE_FILE).exists() {
        return true;
    }
    // Test aid, debug builds only: show the lock screen without the PAM
    // file. It can then only be left by ending the session.
    #[cfg(debug_assertions)]
    if std::env::var_os("MERIDIAN_LOCK_ALLOW_UNINSTALLED").is_some() {
        return true;
    }
    false
}

fn lock(app: &gtk::Application) {
    if !can_authenticate() {
        eprintln!(
            "meridian-lock: {} is missing, so no password could be checked. Not locking. \
             Install the meridian-git package to get it.",
            auth::SERVICE_FILE
        );
        return;
    }
    if !gtk4_session_lock::is_supported() {
        eprintln!("meridian-lock: this compositor does not support session locking. Not locking.");
        return;
    }
    shell::install_css(include_str!("style.css"));

    let session_lock = gtk4_session_lock::Instance::new();
    // The compositor refused (another lock screen is already active).
    session_lock.connect_failed(glib::clone!(
        #[weak]
        app,
        move |_| {
            eprintln!("meridian-lock: the session could not be locked");
            app.quit();
        }
    ));
    session_lock.connect_unlocked(glib::clone!(
        #[weak]
        app,
        move |_| app.quit()
    ));
    if !session_lock.lock() {
        return;
    }

    // One window per monitor, now and for monitors plugged in later.
    let Some(display) = gdk::Display::default() else { return };
    let monitors = display.monitors();
    for index in 0..monitors.n_items() {
        if let Some(monitor) = monitors.item(index).and_downcast::<gdk::Monitor>() {
            let window = build_window(app, &session_lock);
            session_lock.assign_window_to_monitor(&window, &monitor);
        }
    }
    session_lock.connect_monitor(glib::clone!(
        #[weak]
        app,
        move |session_lock, monitor| {
            let window = build_window(&app, session_lock);
            session_lock.assign_window_to_monitor(&window, monitor);
        }
    ));

    // Follow the compositor's mood.
    shell::watch_compositor(|state| shell::apply_mood(&state.theme), || {});
}

fn build_window(app: &gtk::Application, session_lock: &gtk4_session_lock::Instance) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .css_classes(["lock-window"])
        .build();

    // ---- background: the landscape ----------------------------------------
    let background = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
    background.set_draw_func(|_, cr, width, height| {
        let theme = Themes::load().get(Mood::from_name(&shell::current_mood()));
        shell::paint_landscape(cr, width as f64, height as f64, &theme);
    });

    // ---- clock and date ---------------------------------------------------
    let clock = gtk::Label::builder().css_classes(["clock"]).build();
    let date = gtk::Label::builder().css_classes(["date"]).build();
    let update_time = glib::clone!(
        #[weak]
        clock,
        #[weak]
        date,
        move || {
            if let Ok(now) = glib::DateTime::now_local() {
                clock.set_label(&now.format("%H:%M").unwrap_or_default());
                date.set_label(&now.format("%A %-e %B").unwrap_or_default());
            }
        }
    );
    update_time();
    glib::timeout_add_seconds_local(1, move || {
        update_time();
        glib::ControlFlow::Continue
    });

    // ---- the card: who, password, unlock ------------------------------------
    let user = glib::user_name().to_string_lossy().into_owned();
    let real_name = glib::real_name().to_string_lossy().into_owned();
    let shown_name = if real_name.is_empty() || real_name == "Unknown" { user.clone() } else { real_name };
    let initial: String = shown_name.chars().take(1).flat_map(char::to_uppercase).collect();

    let avatar = gtk::Label::builder()
        .label(initial)
        .css_classes(["avatar"])
        .halign(gtk::Align::Center)
        .build();
    let name = gtk::Label::builder().label(&shown_name).css_classes(["user-name"]).build();
    let password = gtk::PasswordEntry::builder()
        .show_peek_icon(true)
        .placeholder_text("Password")
        .css_classes(["password"])
        .build();
    let unlock = gtk::Button::builder().label("Unlock").css_classes(["unlock"]).build();
    let status = gtk::Label::builder().css_classes(["status"]).build();

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["card"])
        .width_request(300)
        .halign(gtk::Align::Center)
        .build();
    for widget in [
        avatar.upcast_ref::<gtk::Widget>(),
        name.upcast_ref(),
        password.upcast_ref(),
        unlock.upcast_ref(),
        status.upcast_ref(),
    ] {
        card.append(widget);
    }

    // ---- power buttons ------------------------------------------------------
    let power = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .vexpand(true)
        .build();
    for (label, verb) in [("Sleep", "suspend"), ("Restart", "reboot"), ("Shut down", "poweroff")] {
        let button = gtk::Button::builder().label(label).css_classes(["power"]).build();
        button.connect_clicked(move |_| {
            // systemd-logind lets the logged-in user do this without root.
            if let Err(err) = std::process::Command::new("systemctl").arg(verb).spawn() {
                eprintln!("meridian-lock: systemctl {verb} failed: {err}");
            }
        });
        power.append(&button);
    }

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .margin_top(70)
        .build();
    column.append(&clock);
    column.append(&date);
    column.append(&card);
    column.append(&power);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&background));
    overlay.add_overlay(&column);
    window.set_child(Some(&overlay));

    // ---- checking the password ----------------------------------------------
    // True while a check is running, so Enter cannot start a second one.
    let checking = Rc::new(Cell::new(false));
    let submit = {
        let session_lock = session_lock.clone();
        let password = password.clone();
        let unlock = unlock.clone();
        let status = status.clone();
        move || {
            if checking.get() {
                return;
            }
            // Take the password out of the field at once. `Zeroizing`
            // overwrites this copy when it is dropped.
            let typed = Zeroizing::new(password.text().to_string());
            password.set_text("");
            if typed.is_empty() {
                return;
            }

            checking.set(true);
            unlock.set_sensitive(false);
            status.set_label("Checking…");

            // PAM can take seconds; do it on a worker thread and get the
            // answer back through a channel.
            let (sender, receiver) = async_channel::bounded::<bool>(1);
            let user = user.clone();
            std::thread::spawn(move || {
                let accepted = auth::check(&user, typed);
                let _ = sender.send_blocking(accepted);
            });

            let session_lock = session_lock.clone();
            let password = password.clone();
            let unlock = unlock.clone();
            let status = status.clone();
            let checking = Rc::clone(&checking);
            glib::spawn_future_local(async move {
                let accepted = receiver.recv().await.unwrap_or(false);
                checking.set(false);
                if accepted {
                    // The `unlocked` signal then quits the program.
                    session_lock.unlock();
                } else {
                    status.set_label("That password is not right. Try again.");
                    unlock.set_sensitive(true);
                    password.grab_focus();
                }
            });
        }
    };
    password.connect_activate({
        let submit = submit.clone();
        move |_| submit()
    });
    unlock.connect_clicked(move |_| submit());

    // Typing should work the moment the lock screen appears.
    window.connect_map(glib::clone!(
        #[weak]
        password,
        move |_| {
            password.grab_focus();
        }
    ));

    window
}
