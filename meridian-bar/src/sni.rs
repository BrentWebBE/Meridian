//! The system tray: icons that background apps put in the bar (chat
//! clients, network tools, cloud sync).
//!
//! The protocol is "StatusNotifierItem", spoken over D-Bus:
//!
//! * An app that wants an icon calls `RegisterStatusNotifierItem` on
//!   whoever owns the name `org.kde.StatusNotifierWatcher`. We are that
//!   **watcher**.
//! * We are also the **host**: for every registered item we read its icon
//!   and title, show a button, and forward clicks to it.
//! * An item's menu is a second D-Bus object ("dbusmenu") that describes
//!   its entries; we read the description and draw the menu ourselves.
//!
//! Everything here is asynchronous: a call to another program never makes
//! the bar wait for the answer.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
/// How long to wait for another program's answer, in milliseconds.
const CALL_TIMEOUT: i32 = 2000;
const ICON_SIZE: i32 = 16;

/// What we tell D-Bus the watcher looks like.
const WATCHER_XML: &str = r#"
<node>
  <interface name="org.kde.StatusNotifierWatcher">
    <method name="RegisterStatusNotifierItem"><arg name="service" type="s" direction="in"/></method>
    <method name="RegisterStatusNotifierHost"><arg name="service" type="s" direction="in"/></method>
    <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
    <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
    <property name="ProtocolVersion" type="i" access="read"/>
    <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
    <signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
    <signal name="StatusNotifierHostRegistered"/>
  </interface>
</node>"#;

/// One tray icon.
struct Item {
    /// The app's bus name and the object path of its item.
    bus: String,
    path: String,
    button: gtk::Button,
    image: gtk::Image,
    /// Where the app's menu lives, if it has one.
    menu: RefCell<Option<String>>,
    /// Kept so the subscriptions end when the item goes away.
    name_watch: RefCell<Option<gio::WatcherId>>,
    signals: RefCell<Option<gio::SignalSubscriptionId>>,
}

struct Tray {
    /// The row of icons in the bar.
    row: gtk::Box,
    /// Items by their id, "busname/path".
    items: RefCell<HashMap<String, Rc<Item>>>,
    bus: RefCell<Option<gio::DBusConnection>>,
}

/// Build the tray widget and start serving the watcher.
pub fn tray() -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["tray"])
        .build();
    let tray = Rc::new(Tray {
        row: row.clone(),
        items: RefCell::new(HashMap::new()),
        bus: RefCell::new(None),
    });

    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |result| match result {
        Ok(bus) => tray.serve(bus),
        Err(err) => eprintln!("meridian-bar: no session bus, the tray is off: {err}"),
    });
    row
}

impl Tray {
    /// Publish the watcher object and claim its well-known name.
    fn serve(self: &Rc<Self>, bus: gio::DBusConnection) {
        *self.bus.borrow_mut() = Some(bus.clone());

        let Some(interface) = gio::DBusNodeInfo::for_xml(WATCHER_XML)
            .ok()
            .and_then(|node| node.lookup_interface(WATCHER_NAME))
        else {
            return;
        };

        let registered = bus
            .register_object(WATCHER_PATH, &interface)
            .method_call({
                let tray = Rc::clone(self);
                move |bus, sender, _path, _interface, method, parameters, invocation| {
                    if method == "RegisterStatusNotifierItem" {
                        let service = parameters.get::<(String,)>().map(|(s,)| s).unwrap_or_default();
                        // Apps pass either their bus name or just the object
                        // path of the item (then the caller is the app).
                        let (name, path) = if service.starts_with('/') {
                            (sender.unwrap_or_default().to_string(), service)
                        } else {
                            (service, "/StatusNotifierItem".to_string())
                        };
                        if !name.is_empty() {
                            tray.add_item(&bus, name, path);
                        }
                    }
                    // `RegisterStatusNotifierHost` needs no action: we are the host.
                    invocation.return_value(None);
                }
            })
            .property({
                let tray = Rc::clone(self);
                move |_bus, _sender, _path, _interface, property| match property {
                    "RegisteredStatusNotifierItems" => {
                        let ids: Vec<String> = tray.items.borrow().keys().cloned().collect();
                        ids.to_variant()
                    }
                    "IsStatusNotifierHostRegistered" => true.to_variant(),
                    _ => 0i32.to_variant(), // ProtocolVersion
                }
            })
            .build();
        if let Err(err) = registered {
            eprintln!("meridian-bar: cannot publish the tray watcher: {err}");
            return;
        }

        // The returned id could be used to give the name up again; we keep
        // it for as long as the bar runs, so it is not needed.
        let _owner = gio::bus_own_name_on_connection(
            &bus,
            WATCHER_NAME,
            gio::BusNameOwnerFlags::NONE,
            |bus, _name| {
                // Apps started before us wait for this signal to register.
                let _ = bus.emit_signal(None, WATCHER_PATH, WATCHER_NAME, "StatusNotifierHostRegistered", None);
            },
            |_bus, _name| eprintln!("meridian-bar: another tray is already running; ours stays empty"),
        );
    }

    fn add_item(self: &Rc<Self>, bus: &gio::DBusConnection, name: String, path: String) {
        let id = format!("{name}{path}");
        if self.items.borrow().contains_key(&id) {
            return;
        }

        let image = gtk::Image::from_icon_name("image-missing-symbolic");
        image.set_pixel_size(ICON_SIZE);
        let button = gtk::Button::builder()
            .child(&image)
            .css_classes(["tray-item"])
            .valign(gtk::Align::Center)
            .build();
        let item = Rc::new(Item {
            bus: name.clone(),
            path: path.clone(),
            button: button.clone(),
            image,
            menu: RefCell::new(None),
            name_watch: RefCell::new(None),
            signals: RefCell::new(None),
        });

        // Left click: the app's main action. Right click: its menu.
        button.connect_clicked({
            let tray = Rc::clone(self);
            let item = Rc::clone(&item);
            move |_| tray.activate(&item)
        });
        let right_click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        right_click.connect_pressed({
            let tray = Rc::clone(self);
            let item = Rc::clone(&item);
            move |_, _, _, _| tray.open_menu(&item)
        });
        button.add_controller(right_click);

        // Remove the icon when the app leaves the bus (quit or crashed).
        let watch = gio::bus_watch_name_on_connection(
            bus,
            &name,
            gio::BusNameWatcherFlags::NONE,
            |_, _, _| {},
            {
                let tray = Rc::clone(self);
                let id = id.clone();
                move |_, _| tray.remove_item(&id)
            },
        );
        *item.name_watch.borrow_mut() = Some(watch);

        // Re-read the item whenever it says something about it changed.
        #[allow(deprecated)] // the replacement API is newer than our minimum GLib
        let signals = bus.signal_subscribe(
            Some(&name),
            Some(ITEM_INTERFACE),
            None, // NewIcon, NewTitle, NewStatus, NewToolTip, ...
            Some(&path),
            None,
            gio::DBusSignalFlags::NONE,
            {
                let tray = Rc::clone(self);
                let item = Rc::downgrade(&item);
                move |_, _, _, _, _, _| {
                    if let Some(item) = item.upgrade() {
                        tray.refresh(&item);
                    }
                }
            },
        );
        *item.signals.borrow_mut() = Some(signals);

        self.row.append(&button);
        self.items.borrow_mut().insert(id.clone(), Rc::clone(&item));
        self.refresh(&item);
        let _ = bus.emit_signal(
            None,
            WATCHER_PATH,
            WATCHER_NAME,
            "StatusNotifierItemRegistered",
            Some(&(id,).to_variant()),
        );
    }

    fn remove_item(&self, id: &str) {
        let Some(item) = self.items.borrow_mut().remove(id) else { return };
        self.row.remove(&item.button);
        if let Some(bus) = self.bus.borrow().as_ref() {
            if let Some(watch) = item.name_watch.borrow_mut().take() {
                gio::bus_unwatch_name(watch);
            }
            if let Some(signals) = item.signals.borrow_mut().take() {
                #[allow(deprecated)]
                bus.signal_unsubscribe(signals);
            }
            let _ = bus.emit_signal(
                None,
                WATCHER_PATH,
                WATCHER_NAME,
                "StatusNotifierItemUnregistered",
                Some(&(id,).to_variant()),
            );
        }
    }

    /// Ask the item for all its properties and update the button.
    fn refresh(self: &Rc<Self>, item: &Rc<Item>) {
        let Some(bus) = self.bus.borrow().clone() else { return };
        let call = bus.call_future(
            Some(&item.bus),
            &item.path,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            Some(&(ITEM_INTERFACE,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT,
        );
        let item = Rc::clone(item);
        glib::spawn_future_local(async move {
            let Ok(reply) = call.await else { return };
            // The reply is a tuple holding one dictionary: name -> value.
            let properties = glib::VariantDict::new(Some(&reply.child_value(0)));
            let text = |key: &str| {
                properties
                    .lookup_value(key, None)
                    .and_then(|value| value.str().map(str::to_string))
                    .unwrap_or_default()
            };

            // "Passive" items ask not to be shown.
            item.button.set_visible(text("Status") != "Passive");
            let title = text("Title");
            item.button
                .set_tooltip_text((!title.is_empty()).then_some(title.as_str()));
            *item.menu.borrow_mut() = Some(text("Menu")).filter(|path| path.starts_with('/') && path != "/");

            // The icon comes as a theme name, or as raw pixels.
            let icon_name = text("IconName");
            let pixmap = properties.lookup_value("IconPixmap", None).and_then(|value| pixmap_texture(&value));
            if !icon_name.is_empty() {
                let theme_path = text("IconThemePath");
                if !theme_path.is_empty()
                    && let Some(display) = gdk::Display::default()
                {
                    gtk::IconTheme::for_display(&display).add_search_path(theme_path);
                }
                item.image.set_icon_name(Some(&icon_name));
            } else if let Some(texture) = pixmap {
                item.image.set_paintable(Some(&texture));
            }
        });
    }

    /// Left click. Apps without a main action show their menu instead.
    fn activate(self: &Rc<Self>, item: &Rc<Item>) {
        let Some(bus) = self.bus.borrow().clone() else { return };
        let call = bus.call_future(
            Some(&item.bus),
            &item.path,
            ITEM_INTERFACE,
            "Activate",
            Some(&(0i32, 0i32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT,
        );
        let tray = Rc::clone(self);
        let item = Rc::clone(item);
        glib::spawn_future_local(async move {
            if call.await.is_err() {
                tray.open_menu(&item);
            }
        });
    }

    /// Fetch the item's menu description and show it.
    fn open_menu(self: &Rc<Self>, item: &Rc<Item>) {
        let Some(bus) = self.bus.borrow().clone() else { return };
        let Some(menu_path) = item.menu.borrow().clone() else { return };
        // Whole tree (parent 0, unlimited depth), all properties.
        let call = bus.call_future(
            Some(&item.bus),
            &menu_path,
            MENU_INTERFACE,
            "GetLayout",
            Some(&(0i32, -1i32, Vec::<String>::new()).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT,
        );
        let item = Rc::clone(item);
        glib::spawn_future_local(async move {
            let Ok(reply) = call.await else { return };
            // (revision, root node)
            let entries = menu_entries(&reply.child_value(1));
            if !entries.is_empty() {
                show_menu(&item, bus, menu_path, entries);
            }
        });
    }
}

/// One line of a tray menu.
#[derive(Debug, Clone, PartialEq)]
struct MenuEntry {
    id: i32,
    label: String,
    enabled: bool,
    separator: bool,
    /// `Some(true)` / `Some(false)` for checkable entries.
    checked: Option<bool>,
    /// How deep in submenus the entry is; drawn as indentation.
    depth: usize,
}

/// Flatten a dbusmenu layout node `(id, properties, children)` into lines.
/// The root itself is not an entry; submenus follow their parent, indented.
fn menu_entries(root: &glib::Variant) -> Vec<MenuEntry> {
    fn walk(node: &glib::Variant, depth: usize, out: &mut Vec<MenuEntry>) {
        if node.n_children() < 3 {
            return;
        }
        let children = node.child_value(2);
        for index in 0..children.n_children() {
            // Each child is wrapped in a variant.
            let child = children.child_value(index);
            let child = child.as_variant().unwrap_or(child);
            if child.n_children() < 3 {
                continue;
            }
            let properties = glib::VariantDict::new(Some(&child.child_value(1)));
            let text = |key: &str| {
                properties
                    .lookup_value(key, None)
                    .and_then(|value| value.str().map(str::to_string))
            };
            let flag = |key: &str| properties.lookup_value(key, None).and_then(|value| value.get::<bool>());

            if flag("visible") == Some(false) {
                continue;
            }
            let checkable = text("toggle-type").is_some_and(|kind| !kind.is_empty());
            let checked = properties
                .lookup_value("toggle-state", None)
                .and_then(|value| value.get::<i32>())
                .map(|state| state == 1);
            out.push(MenuEntry {
                id: child.child_value(0).get::<i32>().unwrap_or(0),
                label: strip_mnemonic(&text("label").unwrap_or_default()),
                enabled: flag("enabled").unwrap_or(true),
                separator: text("type").as_deref() == Some("separator"),
                checked: if checkable { Some(checked.unwrap_or(false)) } else { None },
                depth,
            });
            walk(&child, depth + 1, out);
        }
    }

    let mut entries = Vec::new();
    walk(root, 0, &mut entries);
    entries
}

/// Menu labels mark the keyboard shortcut letter with an underscore
/// ("_Quit"); a real underscore is written twice.
fn strip_mnemonic(label: &str) -> String {
    label.replace("__", "\u{0}").replace('_', "").replace('\u{0}', "_")
}

/// The largest image from an `IconPixmap` property, as a texture.
/// Each image is (width, height, pixels) with pixels as A, R, G, B bytes.
fn pixmap_texture(pixmaps: &glib::Variant) -> Option<gdk::MemoryTexture> {
    let mut best: Option<(i32, i32, Vec<u8>)> = None;
    for index in 0..pixmaps.n_children() {
        let (width, height, data) = pixmaps.child_value(index).get::<(i32, i32, Vec<u8>)>()?;
        let complete = width > 0 && height > 0 && data.len() as i64 >= width as i64 * height as i64 * 4;
        if complete && best.as_ref().is_none_or(|(best_width, _, _)| width > *best_width) {
            best = Some((width, height, data));
        }
    }
    let (width, height, mut data) = best?;
    data.truncate((width * height * 4) as usize);
    // ARGB -> RGBA: move the alpha byte of each pixel to the end.
    for pixel in data.chunks_exact_mut(4) {
        pixel.rotate_left(1);
    }
    Some(gdk::MemoryTexture::new(
        width,
        height,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(data),
        (width * 4) as usize,
    ))
}

/// Show a tray menu: a small panel under the bar at the right. A
/// transparent surface covers the rest of the screen, so a click anywhere
/// else closes the menu. (Same approach as Find; it needs no popup support
/// from the compositor.)
fn show_menu(item: &Rc<Item>, bus: gio::DBusConnection, menu_path: String, entries: Vec<MenuEntry>) {
    let Some(app) = item
        .button
        .root()
        .and_downcast::<gtk::Window>()
        .and_then(|window| window.application())
    else {
        return;
    };

    let window = gtk::Window::builder()
        .application(&app)
        .css_classes(["tray-menu-window"])
        .build();
    window.init_layer_shell();
    window.set_namespace(Some("meridian-tray-menu"));
    window.set_layer(Layer::Overlay);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    // Take the keyboard so arrows, Enter and Escape work.
    window.set_keyboard_mode(KeyboardMode::Exclusive);

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["tray-menu"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .build();

    for entry in entries {
        if entry.separator {
            panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            continue;
        }
        let mark = match entry.checked {
            Some(true) => "\u{2713}  ", // a check mark
            _ => "",
        };
        let label = gtk::Label::builder()
            .label(format!("{mark}{}", entry.label))
            .halign(gtk::Align::Start)
            .margin_start(12 * entry.depth as i32)
            .build();
        let button = gtk::Button::builder()
            .child(&label)
            .css_classes(["tray-menu-item"])
            .sensitive(entry.enabled)
            .build();
        button.connect_clicked({
            let bus = bus.clone();
            let name = item.bus.clone();
            let menu_path = menu_path.clone();
            let window = window.clone();
            move |_| {
                // Tell the app which entry was chosen. No answer is needed.
                let event = (entry.id, "clicked", 0i32.to_variant(), 0u32).to_variant();
                bus.call(
                    Some(&name),
                    &menu_path,
                    MENU_INTERFACE,
                    "Event",
                    Some(&event),
                    None,
                    gio::DBusCallFlags::NONE,
                    CALL_TIMEOUT,
                    gio::Cancellable::NONE,
                    |_| {},
                );
                window.close();
            }
        });
        panel.append(&button);
    }

    let backdrop = gtk::Box::builder().hexpand(true).vexpand(true).build();
    let click_outside = gtk::GestureClick::new();
    click_outside.connect_pressed(glib::clone!(
        #[weak]
        window,
        move |_, _, _, _| window.close()
    ));
    backdrop.add_controller(click_outside);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        window,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(keys);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&backdrop));
    overlay.add_overlay(&panel);
    window.set_child(Some(&overlay));
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonics_are_removed() {
        assert_eq!(strip_mnemonic("_Quit"), "Quit");
        assert_eq!(strip_mnemonic("Save _As..."), "Save As...");
        assert_eq!(strip_mnemonic("snake__case"), "snake_case");
    }

    #[test]
    fn menu_layout_is_flattened() {
        // What a dbusmenu `GetLayout` root looks like, in GVariant text.
        let root = glib::Variant::parse(
            None,
            r#"(0, @a{sv} {}, [
                <(1, {'label': <'_Open'>}, @av [])>,
                <(2, {'type': <'separator'>}, @av [])>,
                <(3, {'label': <'Hidden'>, 'visible': <false>}, @av [])>,
                <(4, {'label': <'Mute'>, 'toggle-type': <'checkmark'>, 'toggle-state': <1>}, @av [])>,
                <(5, {'label': <'More'>, 'enabled': <false>}, [
                    <(6, {'label': <'Inner'>}, @av [])>
                ])>
            ])"#,
        )
        .unwrap();

        let entries = menu_entries(&root);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["Open", "", "Mute", "More", "Inner"]);
        assert!(entries[1].separator);
        assert_eq!(entries[2].checked, Some(true));
        assert_eq!(entries[0].checked, None);
        assert!(!entries[3].enabled);
        assert_eq!(entries[4].depth, 1);
        assert_eq!(entries[4].id, 6);
    }

    #[test]
    fn broken_layouts_give_no_entries() {
        let not_a_node = glib::Variant::parse(None, "(1, 2)").unwrap();
        assert!(menu_entries(&not_a_node).is_empty());
    }
}
