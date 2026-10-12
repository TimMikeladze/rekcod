//! `rekcod menubar`: a menu bar item with the same actions as the CLI
//! (docs/frontends.md). The title follows the VM every 2 s; the menu is
//! built from `actions::Model` each time it opens, and every item runs an
//! `actions::Action` (a `rekcod` command).

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSImage, NSMenu, NSMenuDelegate,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString, NSTimer};

use crate::actions::{self, Action, Model};
use crate::paths::Paths;

const LABEL: &str = "dev.rekcod.menubar";

/// docker's whale, upside down; a template image, so macOS tints it for the menu bar.
const ICON_SVG: &str = include_str!("../assets/menubar-whale.svg");
/// Frames of the busy animation (the whale bobs and rocks), shown every [`FRAME_SECS`].
const FRAMES: usize = 12;
const FRAME_SECS: f64 = 0.08;

/// The icon moved by `dy` points and rocked by `degrees` around its centre.
pub fn icon_frame(dy: f64, degrees: f64) -> String {
    ICON_SVG
        .replacen("<g transform=", &format!("<g transform=\"translate(0 {dy:.2}) rotate({degrees:.2} 11 9)\"><g transform="), 1)
        .replacen("</svg>", "</g></svg>", 1)
}

/// The busy animation: one bob and rock per cycle.
pub fn busy_frames() -> Vec<String> {
    (0..FRAMES)
        .map(|i| {
            let t = i as f64 / FRAMES as f64 * std::f64::consts::TAU;
            icon_frame(1.2 * t.sin(), 7.0 * t.cos())
        })
        .collect()
}

pub struct Ivars {
    paths: Paths,
    item: OnceCell<Retained<NSStatusItem>>,
    /// The actions behind the open menu's items (item tag = index).
    actions: RefCell<Vec<Action>>,
    /// Still image, then the busy animation.
    icon: OnceCell<Retained<NSImage>>,
    frames: OnceCell<Vec<Retained<NSImage>>>,
    frame: Cell<Option<usize>>,
    /// Menu actions still running.
    running_actions: Arc<AtomicUsize>,
    /// The VM is booting or shutting down (daemon alive, docker not answering).
    transitioning: Arc<AtomicBool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RekcodMenuTarget"]
    #[ivars = Ivars]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSMenuDelegate for Target {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            self.fill(menu);
        }
    }

    impl Target {
        #[unsafe(method(tick:))]
        fn tick(&self, _sender: Option<&AnyObject>) {
            self.update_title();
        }

        #[unsafe(method(animate:))]
        fn animate(&self, _sender: Option<&AnyObject>) {
            self.step_animation();
        }

        #[unsafe(method(runAction:))]
        fn run_action(&self, sender: Option<&NSMenuItem>) {
            let Some(item) = sender else { return };
            let action = self.ivars().actions.borrow().get(item.tag() as usize).cloned();
            if let Some(action) = action {
                self.perform(&action);
            }
        }

        #[unsafe(method(openLogs:))]
        fn open_logs(&self, _sender: Option<&AnyObject>) {
            open(&self.ivars().paths.home.join("logs").display().to_string());
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            std::process::exit(0);
        }
    }
);

/// One menu entry, before it becomes an `NSMenuItem` (testable without AppKit).
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Label(String),
    Action(Action),
    Submenu(String, Vec<Entry>),
    Separator,
}

/// The whole menu for `m`: everything comes from the `actions` lists.
pub fn entries(m: &Model) -> Vec<Entry> {
    let mut out = Vec::new();
    let s = &m.status;
    if m.running {
        out.push(Entry::Label(format!("rekcod running · {} CPUs · {} MiB", s["cpus"], s["memory_mib"])));
    } else {
        out.push(Entry::Label("rekcod is stopped".into()));
    }
    let vm = actions::vm_actions(m.running, m.socket_installed);
    out.extend(vm.into_iter().map(Entry::Action));
    if !m.running {
        return out;
    }
    out.push(Entry::Separator);
    if m.ports.is_empty() {
        out.push(Entry::Label("No forwarded ports".into()));
    } else {
        out.push(Entry::Label("Ports".into()));
    }
    for p in &m.ports {
        let name = format!(
            "localhost:{}{} → {}{}",
            p.port,
            if p.udp { "/udp" } else { "" },
            p.container,
            if p.sandbox { " (sandbox)" } else { "" }
        );
        match (&p.error, actions::port_actions(p).into_iter().next()) {
            (Some(e), _) => out.push(Entry::Label(format!("{name}  (not forwarded: {e})"))),
            (None, Some(open)) => out.push(Entry::Action(Action { label: name, ..open })),
            (None, None) => out.push(Entry::Label(name)),
        }
    }
    out.push(Entry::Separator);
    let containers: Vec<Entry> = m
        .containers
        .iter()
        .map(|c| {
            let dot = if c.state == "running" { "●" } else { "○" };
            Entry::Submenu(
                format!("{dot} {}  ({})", c.name, c.image),
                actions::container_actions(c).into_iter().map(Entry::Action).collect(),
            )
        })
        .collect();
    out.push(if containers.is_empty() {
        Entry::Label("No containers".into())
    } else {
        Entry::Submenu(format!("Containers ({})", m.containers.len()), containers)
    });
    let projects: Vec<Entry> = m
        .projects
        .iter()
        .map(|p| {
            let mut items: Vec<Entry> = actions::project_actions(p).into_iter().map(Entry::Action).collect();
            if !p.snapshots.is_empty() {
                items.push(Entry::Separator);
            }
            for s in &p.snapshots {
                let title = format!("{}  ({})", s.name, actions::age(s.created));
                items.push(Entry::Submenu(title, actions::snapshot_actions(s).into_iter().map(Entry::Action).collect()));
            }
            Entry::Submenu(p.name.clone(), items)
        })
        .collect();
    out.push(if projects.is_empty() {
        Entry::Label("No compose projects or snapshots".into())
    } else {
        Entry::Submenu("Snapshots".into(), projects)
    });
    if let Some(sb) = &m.sandbox {
        out.push(Entry::Label(format!("Sandbox VM: {}", sb["project"].as_str().unwrap_or("?"))));
    }
    out
}

impl Target {
    fn new(mtm: MainThreadMarker, paths: Paths) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            paths,
            item: OnceCell::new(),
            actions: RefCell::default(),
            icon: OnceCell::new(),
            frames: OnceCell::new(),
            frame: Cell::new(None),
            running_actions: Arc::default(),
            transitioning: Arc::default(),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn item(&self, text: &str, action: Option<Sel>) -> Retained<NSMenuItem> {
        let mtm = self.mtm();
        let item =
            unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), &NSString::from_str(text), action, &NSString::new()) };
        if action.is_some() {
            unsafe { item.setTarget(Some(self)) };
        } else {
            item.setEnabled(false);
        }
        item
    }

    fn add(&self, menu: &NSMenu, entries: &[Entry]) {
        let mtm = self.mtm();
        for e in entries {
            match e {
                Entry::Label(text) => menu.addItem(&self.item(text, None)),
                Entry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                Entry::Action(a) => {
                    let label = if a.confirm.is_some() || a.interactive { format!("{}…", a.label) } else { a.label.clone() };
                    let item = self.item(&label, Some(sel!(runAction:)));
                    let mut list = self.ivars().actions.borrow_mut();
                    item.setTag(list.len() as isize);
                    list.push(a.clone());
                    menu.addItem(&item);
                }
                Entry::Submenu(title, children) => {
                    let item = self.item(title, None);
                    item.setEnabled(true);
                    let sub = NSMenu::new(mtm);
                    sub.setAutoenablesItems(false);
                    self.add(&sub, children);
                    item.setSubmenu(Some(&sub));
                    menu.addItem(&item);
                }
            }
        }
    }

    /// Rebuilds the menu's items from a fresh model (called as it opens).
    fn fill(&self, menu: &NSMenu) {
        let model = Model::load(&self.ivars().paths);
        menu.removeAllItems();
        self.ivars().actions.borrow_mut().clear();
        self.add(menu, &entries(&model));
        let mtm = self.mtm();
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.item("Open logs folder", Some(sel!(openLogs:))));
        menu.addItem(&self.item("Quit menu bar", Some(sel!(quit:))));
    }

    /// Icon only, no name; dimmed while the VM is stopped.
    fn update_title(&self) {
        if self.ivars().frame.get().is_some() {
            return; // animating
        }
        let mtm = self.mtm();
        let running = crate::cli::running_pid(&self.ivars().paths).is_some();
        if let Some(button) = self.ivars().item.get().and_then(|i| i.button(mtm)) {
            button.setAppearsDisabled(!running);
            button.setToolTip(Some(&NSString::from_str(if running { "rekcod is running" } else { "rekcod is stopped" })));
        }
    }

    fn perform(&self, action: &Action) {
        if let Some(question) = &action.confirm
            && !self.ask(&action.label, question)
        {
            return;
        }
        if action.interactive {
            in_terminal(&action.command_line());
            return;
        }
        // Runs in the background; the next menu open shows the result.
        let mut cmd = action.command();
        cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if let Ok(mut child) = cmd.spawn() {
            // The icon animates until the command is done.
            let running = self.ivars().running_actions.clone();
            running.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let _ = child.wait();
                running.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }

    fn busy(&self) -> bool {
        self.ivars().running_actions.load(Ordering::SeqCst) > 0 || self.ivars().transitioning.load(Ordering::SeqCst)
    }

    /// Next animation frame while busy; back to the still icon when done.
    fn step_animation(&self) {
        let mtm = self.mtm();
        let ivars = self.ivars();
        let (Some(button), Some(frames), Some(icon)) = (ivars.item.get().and_then(|i| i.button(mtm)), ivars.frames.get(), ivars.icon.get())
        else {
            return;
        };
        match (self.busy(), ivars.frame.get()) {
            (true, current) => {
                let next = current.map_or(0, |i| (i + 1) % frames.len());
                ivars.frame.set(Some(next));
                button.setAppearsDisabled(false);
                button.setImage(Some(&frames[next]));
            }
            (false, Some(_)) => {
                ivars.frame.set(None);
                button.setImage(Some(icon));
                self.update_title();
            }
            (false, None) => {}
        }
    }

    fn ask(&self, title: &str, question: &str) -> bool {
        let mtm = self.mtm();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(question));
        alert.addButtonWithTitle(&NSString::from_str(title));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        NSApplication::sharedApplication(mtm).activate();
        alert.runModal() == NSAlertFirstButtonReturn
    }
}

fn open(target: &str) {
    if let Ok(mut child) = Command::new("/usr/bin/open").arg(target).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

/// Opens Terminal.app running `command`.
fn in_terminal(command: &str) {
    let escaped = command.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("tell application \"Terminal\"\nactivate\ndo script \"{escaped}\"\nend tell");
    if let Ok(mut child) = Command::new("/usr/bin/osascript").args(["-e", &script]).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

pub fn run(paths: Paths) -> Result<()> {
    let mtm = MainThreadMarker::new().context("menubar must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let target = Target::new(mtm, paths);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    item.setMenu(Some(&menu));
    let image = |svg: &str| {
        let icon = NSImage::initWithData(mtm.alloc::<NSImage>(), &NSData::with_bytes(svg.as_bytes()))?;
        icon.setSize(NSSize::new(22.0, 18.0));
        icon.setTemplate(true);
        Some(icon)
    };
    if let Some(button) = item.button(mtm) {
        match image(ICON_SVG) {
            Some(icon) => {
                button.setImage(Some(&icon));
                button.setTitle(&NSString::new());
                button.setImagePosition(NSCellImagePosition::ImageOnly);
                let _ = target.ivars().icon.set(icon);
                let frames: Option<Vec<_>> = busy_frames().iter().map(|f| image(f)).collect();
                if let Some(frames) = frames {
                    let _ = target.ivars().frames.set(frames);
                }
            }
            None => button.setTitle(&NSString::from_str("rekcod")),
        }
    }
    // Booting or stopping: the daemon is alive but docker doesn't answer yet.
    // Checked off the main thread (a ping can take a moment).
    {
        let (paths, flag) = (target.ivars().paths.clone(), target.ivars().transitioning.clone());
        std::thread::spawn(move || {
            loop {
                let alive = crate::cli::running_pid(&paths).is_some();
                let answering = alive && crate::cli::api_get(&paths, "/_ping").is_ok_and(|b| b == b"OK");
                flag.store(alive && !answering, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });
    }
    let _ = target.ivars().item.set(item);
    target.update_title();
    let _timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(2.0, &target, sel!(tick:), None, true) };
    let _animation = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(FRAME_SECS, &target, sel!(animate:), None, true)
    };
    app.run();
    Ok(())
}

// ------------------------------------------------------------ login item

fn plist_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(Path::new(&home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// LaunchAgent that starts `<exe> menubar` at login.
pub fn plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>menubar</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(&exe.display().to_string())
    )
}

fn launchctl(args: &[&str]) -> std::process::Output {
    Command::new("/bin/launchctl").args(args).output().unwrap_or_else(|_| std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(1),
        stdout: Vec::new(),
        stderr: Vec::new(),
    })
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

pub fn install() -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let path = plist_path()?;
    std::fs::create_dir_all(path.parent().context("plist dir")?)?;
    // Replace a previous registration (e.g. the binary moved).
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    std::fs::write(&path, plist(&exe))?;
    let out = launchctl(&["bootstrap", &domain(), &path.display().to_string()]);
    if !out.status.success() {
        bail!("launchctl bootstrap: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    println!("menu bar item installed: starts at login ({})", path.display());
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let path = plist_path()?;
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("menu bar item removed ({})", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("menu bar item was not installed"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_menubar_at_login() {
        let p = plist(Path::new("/opt/r&d/rekcod"));
        assert!(p.contains("<string>/opt/r&amp;d/rekcod</string>\n    <string>menubar</string>"));
        assert!(p.contains("<key>RunAtLoad</key><true/>"));
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
    }

    #[test]
    fn busy_frames_move_the_whole_icon() {
        let frames = busy_frames();
        assert_eq!(frames.len(), FRAMES);
        assert!(frames[0].contains("<g transform=\"translate(0 0.00) rotate(7.00 11 9)\"><g transform=\"translate(0 18) scale(1 -1)\">"));
        assert!(frames.iter().all(|f| f.matches("<g").count() == f.matches("</g>").count()));
        assert_ne!(frames[0], frames[FRAMES / 4]);
    }

    #[test]
    fn menu_offers_every_action_kind() {
        let (containers, ports, projects) = crate::actions::tests::sample();
        let model = Model {
            running: true,
            status: serde_json::json!({"cpus": 6, "memory_mib": 8192}),
            containers,
            ports,
            projects,
            sandbox: None,
            socket_installed: false,
        };
        fn kinds(e: &[Entry], out: &mut Vec<crate::actions::Kind>) {
            for e in e {
                match e {
                    Entry::Action(a) => out.push(a.kind),
                    Entry::Submenu(_, c) => kinds(c, out),
                    _ => {}
                }
            }
        }
        let mut seen = Vec::new();
        kinds(&entries(&model), &mut seen);
        kinds(&entries(&Model::default()), &mut seen);
        for k in crate::actions::Kind::ALL {
            assert!(seen.contains(&k), "menu bar lacks {k:?}");
        }
        let top = entries(&model);
        assert_eq!(top[0], Entry::Label("rekcod running · 6 CPUs · 8192 MiB".into()));
        assert!(top.contains(&Entry::Label("Ports".into())));
    }
}
