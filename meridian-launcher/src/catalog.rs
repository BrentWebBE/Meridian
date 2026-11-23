//! Things Find offers besides apps and files: actions (do something now)
//! and settings (change how Terra behaves).

use std::rc::Rc;

use meridian_common::{
    config::{self, Action},
    ipc::Request,
    shell,
};

/// One entry. `run` is what happens when it is chosen.
pub struct Entry {
    pub title: &'static str,
    /// Extra words the entry can be found by.
    pub keywords: &'static str,
    pub run: Rc<dyn Fn()>,
}

fn entry(title: &'static str, keywords: &'static str, run: impl Fn() + 'static) -> Entry {
    Entry {
        title,
        keywords,
        run: Rc::new(run),
    }
}

/// An entry that sends one request to the compositor.
fn request(title: &'static str, keywords: &'static str, action: Action) -> Entry {
    entry(title, keywords, move || shell::send(&Request::Action(action.clone())))
}

/// An entry that runs `systemctl <verb>`. systemd-logind lets the
/// logged-in user do this without being root.
fn power(title: &'static str, keywords: &'static str, verb: &'static str) -> Entry {
    entry(title, keywords, move || {
        if let Err(err) = std::process::Command::new("systemctl").arg(verb).spawn() {
            eprintln!("meridian-launcher: systemctl {verb} failed: {err}");
        }
    })
}

pub fn actions() -> Vec<Entry> {
    vec![
        request("Open the overview", "windows spaces expose", Action::ToggleOverview),
        request("Open a terminal", "console shell command line", Action::SpawnTerminal),
        request("Take a screenshot", "capture picture screen print", Action::Screenshot),
        request("Switch between floating and tiled windows", "layout tile float arrange", Action::ToggleLayout),
        request("Go to space 1", "workspace desktop", Action::Workspace(1)),
        request("Go to space 2", "workspace desktop", Action::Workspace(2)),
        request("Go to space 3", "workspace desktop", Action::Workspace(3)),
        request("Go to space 4", "workspace desktop", Action::Workspace(4)),
        request("Log out", "quit exit end session", Action::Quit),
        power("Sleep", "suspend standby", "suspend"),
        power("Restart", "reboot", "reboot"),
        power("Shut down", "power off halt", "poweroff"),
    ]
}

/// Write settings to the config file and ask the compositor to apply them.
fn apply(changes: &[(&str, &str, toml_value::Value)]) {
    for (section, key, value) in changes {
        if let Err(err) = config::write_setting(Some(section), key, value.clone()) {
            eprintln!("meridian-launcher: {err}");
            return;
        }
    }
    shell::send(&Request::ReloadConfig);
}

/// A setting that pins one mood.
fn mood(title: &'static str, keywords: &'static str, name: &'static str) -> Entry {
    entry(title, keywords, move || {
        apply(&[
            ("appearance", "theme", name.into()),
            ("appearance", "follow-time-of-day", false.into()),
        ]);
    })
}

pub fn settings() -> Vec<Entry> {
    vec![
        mood("Morning mood", "appearance theme light day", "morning"),
        mood("Dusk mood", "appearance theme evening sunset", "dusk"),
        mood("Night mood", "appearance theme dark", "night"),
        entry("Follow the time of day", "appearance theme automatic mood", || {
            apply(&[("appearance", "follow-time-of-day", true.into())]);
        }),
    ]
}

/// The value type `write_setting` takes, under a shorter name.
mod toml_value {
    pub use meridian_common::config::SettingValue as Value;
}
