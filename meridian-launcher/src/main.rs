//! Find: Meridian's launcher. A search field in the middle of the screen
//! that finds apps and recent files, lists what is running, and holds the
//! power menu.
//!
//! The program stays running in the background after its first start.
//! Starting it again (Super, Mod+Space, or the logo button in the bar)
//! reaches the running copy and toggles Find, so opening it is instant.
//!
//! While the compositor's overview is open it shows as a slim search field
//! at the top instead, leaving the window thumbnails clickable.

mod apps;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use meridian_common::{
    config::{Action, Config},
    ipc::{Connection, Request, State},
    theme,
};

use crate::apps::AppEntry;

const PANEL_WIDTH: i32 = 580;
const MAX_APPS: usize = 7;
const MAX_FILES: usize = 3;

/// The two looks of the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Centered over a dimmed desktop.
    Find,
    /// Only a search field, top center, shown with the overview.
    Overview,
}

fn main() -> glib::ExitCode {
    // A GTK application id makes the program single-instance: a second
    // start does not open a second copy but sends "activate" to the first.
    let app = gtk::Application::builder()
        .application_id("org.meridian.Launcher")
        .build();

    // `--background` (used by autostart): start without showing anything,
    // ready to react to the overview.
    let start_hidden = std::env::args().any(|arg| arg == "--background");

    app.connect_activate(move |app| match app.windows().first() {
        // Already running: toggle.
        Some(window) if window.is_visible() => window.set_visible(false),
        Some(window) => window.present(),
        None => {
            let window = build_ui(app);
            if !start_hidden {
                window.present();
            }
        }
    });

    // Without a "hold" GTK would quit as soon as Find is hidden. `startup`
    // only runs in the first copy; a second start must not hold, or it would
    // linger after passing on its "activate".
    app.connect_startup(|app| std::mem::forget(app.hold()));
    // Our flag is not for GTK, so do not pass the command line on.
    app.run_with_args::<&str>(&[])
}

/// Something Find can open.
#[derive(Clone)]
enum Target {
    App(gio::AppInfo),
    File(String), // a URI
}

/// Everything the callbacks share. `Rc<Ui>` is handed to each of them.
struct Ui {
    window: gtk::ApplicationWindow,
    backdrop: gtk::Box,
    panel: gtk::Box,
    search: gtk::Entry,
    /// Divider + result list; hidden in the overview until you type.
    body: gtk::Box,
    results: gtk::Box,
    running: gtk::Box,
    footer: gtk::Stack,

    apps: Vec<AppEntry>,
    /// Lowercase desktop ids of the pinned apps, shown first.
    pinned: Vec<String>,
    /// The rows currently listed, top to bottom.
    rows: RefCell<Vec<(gtk::Button, Target)>>,
    selected: Cell<usize>,
    mode: Cell<Mode>,
    /// What the compositor last told us about its overview.
    overview_open: Cell<bool>,
}

fn build_ui(app: &gtk::Application) -> gtk::ApplicationWindow {
    load_css();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .css_classes(["launcher-window"])
        .build();
    window.init_layer_shell();
    window.set_namespace(Some("meridian-launcher"));
    window.set_layer(Layer::Top);
    window.set_anchor(Edge::Top, true);
    // Ask the compositor for all keyboard input while Find is open.
    window.set_keyboard_mode(KeyboardMode::Exclusive);

    let config = Config::load().unwrap_or_else(|err| {
        eprintln!("meridian-launcher: {err}; using defaults");
        Config::default()
    });

    // ---- search row -------------------------------------------------------
    let search = gtk::Entry::builder()
        .placeholder_text("Find apps and files")
        .css_classes(["search"])
        .hexpand(true)
        .build();
    let escape = gtk::Button::builder()
        .label("Esc")
        .css_classes(["chip"])
        .valign(gtk::Align::Center)
        .focusable(false)
        .build();
    let search_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .css_classes(["search-row"])
        .build();
    search_row.append(&gtk::Image::from_icon_name("system-search-symbolic"));
    search_row.append(&search);
    search_row.append(&escape);

    // ---- results ----------------------------------------------------------
    let results = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["results"])
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&divider());
    body.append(&results);

    // ---- footer -----------------------------------------------------------
    let running = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let footer = footer(&window, &running);

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["panel"])
        .width_request(PANEL_WIDTH)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .build();
    panel.append(&search_row);
    panel.append(&body);
    panel.append(&footer);

    // An Overlay stacks widgets: the click-catching backdrop below, the panel on top.
    let backdrop = gtk::Box::builder()
        .hexpand(true)
        .vexpand(true)
        .css_classes(["backdrop"])
        .build();
    let click_outside = gtk::GestureClick::new();
    click_outside.connect_pressed(glib::clone!(
        #[weak]
        window,
        move |_, _, _, _| window.set_visible(false)
    ));
    backdrop.add_controller(click_outside);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&backdrop));
    overlay.add_overlay(&panel);
    // Let the window be as large as the panel wants (overview mode).
    overlay.set_measure_overlay(&panel, true);
    window.set_child(Some(&overlay));

    let ui = Rc::new(Ui {
        window: window.clone(),
        backdrop,
        panel,
        search: search.clone(),
        body,
        results,
        running,
        footer,
        apps: apps::installed(),
        pinned: config.dock.pinned.iter().map(|id| id.to_lowercase()).collect(),
        rows: RefCell::new(Vec::new()),
        selected: Cell::new(0),
        mode: Cell::new(Mode::Find),
        overview_open: Cell::new(false),
    });
    ui.set_mode(Mode::Find);
    ui.refresh();

    // ---- behavior ---------------------------------------------------------
    search.connect_changed({
        let ui = Rc::clone(&ui);
        move |_| ui.refresh()
    });
    // Enter opens the highlighted row.
    search.connect_activate({
        let ui = Rc::clone(&ui);
        move |_| ui.open_selected()
    });
    escape.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.set_visible(false)
    ));

    // Escape closes; Up and Down move the highlight. The "capture" phase
    // lets us see keys before the text field uses them.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let ui = Rc::clone(&ui);
        move |_, key, _, _| match key {
            gdk::Key::Escape => {
                ui.window.set_visible(false);
                glib::Propagation::Stop
            }
            gdk::Key::Down => {
                ui.move_selection(1);
                glib::Propagation::Stop
            }
            gdk::Key::Up => {
                ui.move_selection(-1);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);

    // Every time Find opens: empty search field, cursor in it.
    window.connect_show({
        let ui = Rc::clone(&ui);
        move |_| {
            ui.search.set_text("");
            ui.refresh();
            ui.search.grab_focus();
        }
    });

    // Whenever Find closes it returns to its normal look. If it was the
    // overview's search field (Escape, or something was opened), the
    // overview closes with it.
    window.connect_hide({
        let ui = Rc::clone(&ui);
        move |_| {
            let closing_overview = ui.mode.get() == Mode::Overview && ui.overview_open.get();
            ui.set_mode(Mode::Find);
            if closing_overview {
                send(&Request::Action(Action::ToggleOverview));
            }
        }
    });

    watch_compositor(app, ui);
    window
}

impl Ui {
    fn set_mode(&self, mode: Mode) {
        self.mode.set(mode);
        let find = mode == Mode::Find;

        // Find: a dimmed surface over the whole desktop with the panel in
        // its upper middle. Overview: only as large as the panel, top center,
        // so clicks on thumbnails reach the compositor.
        for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
            self.window.set_anchor(edge, find);
        }
        // -1: ignore the space reserved by bar and dock and cover the whole
        // screen, so the dimming has no gaps. 0: stay below the bar.
        self.window.set_exclusive_zone(if find { -1 } else { 0 });
        self.panel.set_margin_top(if find { 124 } else { 8 });
        if find {
            self.backdrop.add_css_class("dim");
        } else {
            self.backdrop.remove_css_class("dim");
        }
        self.footer.set_visible(find);
        self.refresh();
    }

    /// Rebuild the result list for the current query.
    fn refresh(&self) {
        let query = self.search.text().to_string();
        let searching = !query.trim().is_empty();

        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
        let mut rows = Vec::new();

        // Files: recently used ones whose name matches. Only while searching.
        if searching {
            let files = apps::recent_files(&query, MAX_FILES);
            if !files.is_empty() {
                self.results.append(&section_label("Files"));
            }
            for file in files {
                let row = result_row(&file.name, &file.folder);
                self.results.append(&row);
                rows.push((row, Target::File(file.uri)));
            }
        }

        // Apps: matches, or the pinned apps when the field is empty.
        let apps: Vec<&AppEntry> = if searching {
            self.apps.iter().filter(|app| app.matches(&query)).take(MAX_APPS).collect()
        } else {
            self.pinned
                .iter()
                .filter_map(|id| self.apps.iter().find(|app| &app.id == id))
                .take(MAX_APPS)
                .collect()
        };
        if !apps.is_empty() {
            self.results.append(&section_label(if searching { "Apps" } else { "Pinned" }));
        }
        for app in apps {
            let row = result_row(&app.name, "App");
            self.results.append(&row);
            rows.push((row, Target::App(app.info.clone())));
        }

        if rows.is_empty() {
            self.results.append(
                &gtk::Label::builder()
                    .label("Nothing found")
                    .css_classes(["empty"])
                    .build(),
            );
        }

        *self.rows.borrow_mut() = rows;
        self.select(0);
        // In the overview the list only appears once you type.
        self.body.set_visible(self.mode.get() == Mode::Find || searching);
    }

    /// Wire up clicks. Called by `watch_compositor` setup; rows are rebuilt
    /// often, so each click looks its target up by position at that moment.
    fn connect_rows(self: &Rc<Self>) {
        for (index, (row, _)) in self.rows.borrow().iter().enumerate() {
            let ui = Rc::clone(self);
            row.connect_clicked(move |_| {
                ui.select(index);
                ui.open_selected();
            });
        }
    }

    fn select(&self, index: usize) {
        let rows = self.rows.borrow();
        for (row, _) in rows.iter() {
            row.remove_css_class("selected");
        }
        if let Some((row, _)) = rows.get(index) {
            row.add_css_class("selected");
            self.selected.set(index);
        }
    }

    fn move_selection(&self, step: isize) {
        let count = self.rows.borrow().len() as isize;
        if count > 0 {
            // `rem_euclid` wraps around at both ends.
            self.select((self.selected.get() as isize + step).rem_euclid(count) as usize);
        }
    }

    fn open_selected(&self) {
        // Clone the target out so the borrow ends before the window hides
        // (hiding rebuilds the rows).
        let target = self.rows.borrow().get(self.selected.get()).map(|(_, target)| target.clone());
        let Some(target) = target else { return };

        // The launch context passes focus and startup information along.
        let context = gdk::Display::default().map(|display| display.app_launch_context());
        let result = match &target {
            Target::App(info) => info.launch(&[], context.as_ref()),
            Target::File(uri) => gio::AppInfo::launch_default_for_uri(uri, context.as_ref()),
        };
        if let Err(err) = result {
            eprintln!("meridian-launcher: could not open it: {err}");
        }
        self.window.set_visible(false);
    }

    /// The "Running" row: one icon per running app; a click brings it forward.
    fn show_running(&self, state: &State) {
        while let Some(child) = self.running.first_child() {
            self.running.remove(&child);
        }
        // One entry per app: its first window.
        let mut seen: HashMap<String, u32> = HashMap::new();
        let mut order = Vec::new();
        for window in &state.windows {
            let key = window.app_id.to_lowercase();
            if !key.is_empty() && !seen.contains_key(&key) {
                seen.insert(key.clone(), window.id);
                order.push(key);
            }
        }

        for key in order {
            let entry = self.apps.iter().find(|app| app.id == key);
            let image = match entry.and_then(|app| app.info.icon()) {
                Some(icon) => gtk::Image::from_gicon(&icon),
                None => gtk::Image::from_icon_name("application-x-executable"),
            };
            image.set_pixel_size(20);
            let button = gtk::Button::builder()
                .child(&image)
                .css_classes(["running"])
                .tooltip_text(entry.map(|app| app.name.clone()).unwrap_or(key.clone()))
                .focusable(false)
                .build();
            let id = seen[&key];
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = window)]
                self.window,
                move |_| {
                    send(&Request::FocusWindow { id });
                    window.set_visible(false);
                }
            ));
            self.running.append(&button);
        }
    }

    fn follow_overview(&self, open: bool) {
        let was_open = self.overview_open.replace(open);
        if open && !was_open {
            self.window.set_visible(false); // close Find if it was open
            self.set_mode(Mode::Overview);
            self.window.present();
        } else if !open && was_open && self.mode.get() == Mode::Overview {
            self.window.set_visible(false);
        }
    }
}

/// Follow the compositor: keep the "Running" row current, show the search
/// field while the overview is open, and quit when the compositor goes away
/// (this program idles in the background, so it would otherwise outlive the
/// session).
fn watch_compositor(app: &gtk::Application, ui: Rc<Ui>) {
    // Rows are rebuilt on every keystroke; connect their clicks each time.
    ui.search.connect_changed({
        let ui = Rc::clone(&ui);
        move |_| ui.connect_rows()
    });
    ui.window.connect_show({
        let ui = Rc::clone(&ui);
        move |_| ui.connect_rows()
    });
    ui.connect_rows();

    let Ok(connection) = Connection::connect() else {
        return; // started by hand outside Meridian: nothing to watch
    };
    let fd = connection.raw_fd();
    let connection = RefCell::new(connection);
    let app = app.clone();
    glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN | glib::IOCondition::HUP, move |_, _| {
        // Bind the result first so the `RefCell` borrow ends before we act on it.
        let received = connection.borrow_mut().receive::<State>();
        match received {
            Ok(states) => {
                if let Some(state) = states.last() {
                    ui.show_running(state);
                    ui.follow_overview(state.overview);
                }
                glib::ControlFlow::Continue
            }
            Err(_) => {
                app.quit();
                glib::ControlFlow::Break
            }
        }
    });
}

/// Send one request to the compositor.
fn send(request: &Request) {
    let sent = Connection::connect().and_then(|mut connection| connection.send(request));
    if let Err(err) = sent {
        eprintln!("meridian-launcher: could not reach the compositor: {err}");
    }
}

fn load_css() {
    let css = format!("{}\n{}", theme::LIGHT.css_variables(), include_str!("style.css"));
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

fn divider() -> gtk::Box {
    gtk::Box::builder().css_classes(["divider"]).build()
}

fn section_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["section"])
        .halign(gtk::Align::Start)
        .build()
}

/// One line in the list: the name on the left, what it is on the right.
fn result_row(name: &str, kind: &str) -> gtk::Button {
    let name = gtk::Label::builder()
        .label(name)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let kind = gtk::Label::builder().label(kind).css_classes(["kind"]).build();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.append(&name);
    content.append(&kind);

    gtk::Button::builder()
        .child(&content)
        .css_classes(["result"])
        .focusable(false) // the text field keeps the keyboard
        .build()
}

/// Bottom row: running apps and "Power", which swaps in the power actions.
fn footer(window: &gtk::ApplicationWindow, running: &gtk::Box) -> gtk::Stack {
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();

    // Page 1: what is running.
    let label = gtk::Label::builder().label("Running").css_classes(["footer-label"]).build();
    running.set_hexpand(true);
    let power = gtk::Button::builder().label("Power").css_classes(["plain"]).focusable(false).build();
    let running_page = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    running_page.add_css_class("footer");
    running_page.append(&label);
    running_page.append(running);
    running_page.append(&power);

    // Page 2: power actions.
    let power_page = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    power_page.add_css_class("footer");
    let plain = |label: &str| gtk::Button::builder().label(label).css_classes(["plain"]).focusable(false).build();
    let back = plain("Back");
    back.set_hexpand(true);
    back.set_halign(gtk::Align::Start);
    power_page.append(&back);

    let log_out = plain("Log out");
    // Ending the session means asking the compositor to quit.
    log_out.connect_clicked(|_| send(&Request::Action(Action::Quit)));
    power_page.append(&log_out);

    for (label, verb) in [("Sleep", "suspend"), ("Restart", "reboot"), ("Shut down", "poweroff")] {
        let button = plain(label);
        button.connect_clicked(glib::clone!(
            #[weak]
            window,
            move |_| {
                // systemd-logind lets the logged-in user do this without root.
                if let Err(err) = std::process::Command::new("systemctl").arg(verb).spawn() {
                    eprintln!("meridian-launcher: systemctl {verb} failed: {err}");
                }
                window.set_visible(false);
            }
        ));
        power_page.append(&button);
    }

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&divider());
    let pages = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    pages.add_named(&running_page, Some("running"));
    pages.add_named(&power_page, Some("power"));
    outer.append(&pages);
    stack.add_named(&outer, Some("footer"));

    power.connect_clicked(glib::clone!(
        #[weak]
        pages,
        move |_| pages.set_visible_child_name("power")
    ));
    back.connect_clicked(glib::clone!(
        #[weak]
        pages,
        move |_| pages.set_visible_child_name("running")
    ));
    // Always start on the running page.
    window.connect_show(glib::clone!(
        #[weak]
        pages,
        move |_| pages.set_visible_child_name("running")
    ));

    stack
}
