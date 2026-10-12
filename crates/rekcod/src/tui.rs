//! `rekcod tui` (or bare `rekcod` in a terminal): the VM, containers, ports
//! and snapshots on one screen. Everything it can do comes from `actions`
//! and runs as a `rekcod` command (docs/frontends.md).

use std::io::Stdout;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Tabs};

use crate::actions::{self, Action, Model};
use crate::paths::Paths;

const REFRESH: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Containers,
    Ports,
    Snapshots,
}

const TABS: [Tab; 3] = [Tab::Containers, Tab::Ports, Tab::Snapshots];

/// A row of the Snapshots tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapRow {
    Project(usize),
    Snapshot(usize, usize),
}

enum Popup {
    Menu { title: String, actions: Vec<Action>, state: ListState },
    Confirm(Action),
}

pub struct App {
    pub model: Model,
    pub tab: Tab,
    selected: [usize; 3],
    popup: Option<Popup>,
    /// (text, is error, when)
    message: Option<(String, bool, Instant)>,
    busy: Option<String>,
    quit: bool,
}

enum Msg {
    Model(Box<Model>),
    Done { label: String, ok: bool, output: String },
}

impl App {
    pub fn new(model: Model) -> App {
        App { model, tab: Tab::Containers, selected: [0; 3], popup: None, message: None, busy: None, quit: false }
    }

    fn tab_index(&self) -> usize {
        TABS.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    fn snap_rows(&self) -> Vec<SnapRow> {
        let mut rows = Vec::new();
        for (i, p) in self.model.projects.iter().enumerate() {
            rows.push(SnapRow::Project(i));
            rows.extend((0..p.snapshots.len()).map(|j| SnapRow::Snapshot(i, j)));
        }
        rows
    }

    fn row_count(&self) -> usize {
        match self.tab {
            Tab::Containers => self.model.containers.len(),
            Tab::Ports => self.model.ports.len(),
            Tab::Snapshots => self.snap_rows().len(),
        }
    }

    fn selected(&self) -> usize {
        self.selected[self.tab_index()].min(self.row_count().saturating_sub(1))
    }

    /// The selected row's title and actions (straight from `actions`).
    pub fn row_actions(&self) -> Option<(String, Vec<Action>)> {
        let i = self.selected();
        match self.tab {
            Tab::Containers => self.model.containers.get(i).map(|c| (c.name.clone(), actions::container_actions(c))),
            Tab::Ports => self.model.ports.get(i).map(|p| (format!("port {}", p.port), actions::port_actions(p))),
            Tab::Snapshots => match self.snap_rows().get(i)? {
                SnapRow::Project(p) => {
                    let p = &self.model.projects[*p];
                    Some((p.name.clone(), actions::project_actions(p)))
                }
                SnapRow::Snapshot(p, s) => {
                    let s = &self.model.projects[*p].snapshots[*s];
                    Some((format!("{} / {}", s.project, s.name), actions::snapshot_actions(s)))
                }
            },
        }
    }

    fn vm_menu(&mut self) {
        let actions = actions::vm_actions(self.model.running);
        self.popup = Some(Popup::Menu { title: "rekcod VM".into(), actions, state: ListState::default().with_selected(Some(0)) });
    }

    fn row_menu(&mut self) {
        if let Some((title, actions)) = self.row_actions().filter(|(_, a)| !a.is_empty()) {
            self.popup = Some(Popup::Menu { title, actions, state: ListState::default().with_selected(Some(0)) });
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let n = self.row_count();
        let i = self.tab_index();
        if n == 0 {
            self.selected[i] = 0;
            return;
        }
        self.selected[i] = (self.selected() as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    /// Handles a key; returns an action to run.
    pub fn key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return None;
        }
        match self.popup.take() {
            Some(Popup::Confirm(action)) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => return Some(action),
                _ => return None,
            },
            Some(Popup::Menu { title, actions, mut state }) => {
                let chosen = match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        state.select_previous();
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        state.select_next();
                        None
                    }
                    KeyCode::Enter => state.selected().and_then(|i| actions.get(i).cloned()),
                    KeyCode::Char(c) => actions.iter().find(|a| a.kind.key() == c).cloned(),
                    _ => None,
                };
                match chosen {
                    Some(a) => return self.confirm_or_run(a),
                    None => {
                        let len = actions.len();
                        if let Some(i) = state.selected() {
                            state.select(Some(i.min(len.saturating_sub(1))));
                        }
                        self.popup = Some(Popup::Menu { title, actions, state });
                        return None;
                    }
                }
            }
            None => {}
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::Right => self.tab = TABS[(self.tab_index() + 1) % TABS.len()],
            KeyCode::BackTab | KeyCode::Left => self.tab = TABS[(self.tab_index() + TABS.len() - 1) % TABS.len()],
            KeyCode::Char(d @ '1'..='3') => self.tab = TABS[d as usize - '1' as usize],
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter => self.row_menu(),
            KeyCode::Char('v') => self.vm_menu(),
            KeyCode::Char(c) => {
                let action = self.row_actions().and_then(|(_, a)| a.into_iter().find(|a| a.kind.key() == c));
                if let Some(a) = action {
                    return self.confirm_or_run(a);
                }
            }
            _ => {}
        }
        None
    }

    fn confirm_or_run(&mut self, action: Action) -> Option<Action> {
        if action.confirm.is_some() {
            self.popup = Some(Popup::Confirm(action));
            None
        } else {
            Some(action)
        }
    }

    fn hints(&self) -> String {
        let mut parts: Vec<String> = self
            .row_actions()
            .map(|(_, a)| a.iter().map(|a| format!("{} {}", a.kind.key(), short(&a.label))).collect())
            .unwrap_or_default();
        parts.push("⏎ menu".into());
        parts.push(if self.model.running { "v VM" } else { "v start VM" }.into());
        parts.push("tab switch".into());
        parts.push("q quit".into());
        parts.join("  ")
    }

    pub fn draw(&mut self, f: &mut ratatui::Frame) {
        let [header, tabs, body, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Length(1), Constraint::Min(3), Constraint::Length(2)]).areas(f.area());
        f.render_widget(Paragraph::new(header_lines(&self.model)).block(Block::default().borders(Borders::BOTTOM)), header);
        let titles: Vec<String> = vec![
            format!("1 Containers ({})", self.model.containers.len()),
            format!("2 Ports ({})", self.model.ports.len()),
            format!("3 Snapshots ({})", self.model.projects.iter().map(|p| p.snapshots.len()).sum::<usize>()),
        ];
        f.render_widget(
            Tabs::new(titles).select(self.tab_index()).highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)),
            tabs,
        );
        let selected = self.selected();
        let highlight = Style::default().add_modifier(Modifier::REVERSED);
        let mut state = TableState::default().with_selected(Some(selected));
        match self.tab {
            Tab::Containers => {
                let rows = self.model.containers.iter().map(|c| {
                    let color = if c.state == "running" { Color::Green } else { Color::DarkGray };
                    let ports: Vec<String> = c.ports.iter().map(u16::to_string).collect();
                    Row::new(vec![
                        Cell::from(c.name.clone()),
                        Cell::from(c.state.clone()).style(Style::default().fg(color)),
                        Cell::from(c.image.clone()),
                        Cell::from(ports.join(",")),
                        Cell::from(c.project.clone().unwrap_or_default()),
                        Cell::from(c.status.clone()),
                    ])
                });
                let t = Table::new(rows, [Constraint::Percentage(22), Constraint::Length(9), Constraint::Percentage(24), Constraint::Length(12), Constraint::Percentage(14), Constraint::Fill(1)])
                    .header(Row::new(["NAME", "STATE", "IMAGE", "PORTS", "PROJECT", "STATUS"]).style(Style::default().add_modifier(Modifier::BOLD)))
                    .row_highlight_style(highlight);
                f.render_stateful_widget(t, body, &mut state);
            }
            Tab::Ports => {
                let rows = self.model.ports.iter().map(|p| {
                    let note = match (&p.error, p.sandbox) {
                        (Some(e), _) => format!("not forwarded: {e}"),
                        (None, true) => "sandbox VM".into(),
                        (None, false) => String::new(),
                    };
                    Row::new(vec![format!("localhost:{}{}", p.port, if p.udp { "/udp" } else { "" }), p.container.clone(), note])
                });
                let t = Table::new(rows, [Constraint::Length(22), Constraint::Percentage(35), Constraint::Fill(1)])
                    .header(Row::new(["ADDRESS", "CONTAINER", "NOTE"]).style(Style::default().add_modifier(Modifier::BOLD)))
                    .row_highlight_style(highlight);
                f.render_stateful_widget(t, body, &mut state);
            }
            Tab::Snapshots => {
                let rows = self.snap_rows().into_iter().map(|r| match r {
                    SnapRow::Project(i) => {
                        let p = &self.model.projects[i];
                        let dir = p.dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "(no containers)".into());
                        Row::new(vec![p.name.clone(), String::new(), dir]).style(Style::default().add_modifier(Modifier::BOLD))
                    }
                    SnapRow::Snapshot(i, j) => {
                        let s = &self.model.projects[i].snapshots[j];
                        Row::new(vec![format!("  {}", s.name), actions::age(s.created), s.volumes.join(", ")])
                    }
                });
                let t = Table::new(rows, [Constraint::Percentage(30), Constraint::Length(10), Constraint::Fill(1)])
                    .header(Row::new(["PROJECT / SNAPSHOT", "SAVED", "VOLUMES / DIRECTORY"]).style(Style::default().add_modifier(Modifier::BOLD)))
                    .row_highlight_style(highlight);
                f.render_stateful_widget(t, body, &mut state);
            }
        }
        if self.row_count() == 0 {
            let empty = match (self.tab, self.model.running) {
                (_, false) => "rekcod is stopped. Press v to start it.",
                (Tab::Containers, _) => "No containers.",
                (Tab::Ports, _) => "No forwarded ports.",
                (Tab::Snapshots, _) => "No compose projects or snapshots.",
            };
            f.render_widget(Paragraph::new(empty).style(Style::default().fg(Color::DarkGray)), body.inner(ratatui::layout::Margin::new(1, 1)));
        }
        let status = match (&self.busy, &self.message) {
            (Some(b), _) => Line::from(Span::styled(format!("… {b}"), Style::default().fg(Color::Yellow))),
            (None, Some((m, err, _))) => Line::from(Span::styled(m.clone(), Style::default().fg(if *err { Color::Red } else { Color::Green }))),
            _ => Line::default(),
        };
        f.render_widget(Paragraph::new(vec![status, Line::from(Span::styled(self.hints(), Style::default().fg(Color::DarkGray)))]), footer);

        match &mut self.popup {
            Some(Popup::Menu { title, actions, state }) => {
                let items: Vec<ListItem> = actions.iter().map(|a| ListItem::new(format!("{}  {}", a.kind.key(), a.label))).collect();
                let area = centered(f.area(), 50, actions.len() as u16 + 2);
                f.render_widget(Clear, area);
                let list = List::new(items).block(Block::bordered().title(format!(" {title} "))).highlight_style(highlight);
                f.render_stateful_widget(list, area, state);
            }
            Some(Popup::Confirm(a)) => {
                let question = a.confirm.clone().unwrap_or_default();
                let area = centered(f.area(), 60, 5);
                f.render_widget(Clear, area);
                f.render_widget(
                    Paragraph::new(vec![Line::from(question), Line::default(), Line::from("y / ⏎ yes    any other key: no")])
                        .wrap(ratatui::widgets::Wrap { trim: true })
                        .block(Block::bordered().title(format!(" {} ", a.label))),
                    area,
                );
            }
            None => {}
        }
    }
}

fn short(label: &str) -> &str {
    label.split(" (").next().unwrap_or(label)
}

fn centered(area: Rect, width_pct: u16, height: u16) -> Rect {
    let w = area.width * width_pct / 100;
    let h = height.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn gib(v: &serde_json::Value) -> String {
    v.as_u64().map(|b| format!("{:.1}", b as f64 / (1u64 << 30) as f64)).unwrap_or_else(|| "?".into())
}

pub fn header_lines(m: &Model) -> Vec<Line<'static>> {
    let s = &m.status;
    if !m.running {
        return vec![
            Line::from(vec![Span::styled("○ rekcod ", Style::default().add_modifier(Modifier::BOLD)), Span::raw("stopped")]),
            Line::from(Span::styled("press v to start the VM", Style::default().fg(Color::DarkGray))),
        ];
    }
    let trips: u64 = s["fs_requests"].as_object().map(|o| o.values().filter_map(|n| n.as_u64()).sum()).unwrap_or(0);
    let mut second = format!(
        "disk {} of {} GiB ({} GiB on the Mac) · {} trips to macOS since boot",
        gib(&s["disk"]["used_in_vm"]),
        gib(&s["disk"]["size"]),
        gib(&s["disk"]["on_mac"]),
        trips
    );
    if let Some(n) = s["warming"].as_array().map(Vec::len).filter(|n| *n > 0) {
        second.push_str(&format!(" · warming {n}"));
    }
    if let Some(sb) = &m.sandbox {
        second.push_str(&format!(" · sandbox: {}", sb["project"].as_str().unwrap_or("?")));
    }
    vec![
        Line::from(vec![
            Span::styled("● rekcod ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "running · {} CPUs · {} MiB · dockerd {} · {}",
                s["cpus"],
                s["memory_mib"],
                s["dockerd"].as_str().unwrap_or("?"),
                if s["fs_cache"] == true { "cached file sharing" } else { "uncached file sharing" }
            )),
        ]),
        Line::from(Span::styled(second, Style::default().fg(Color::DarkGray))),
    ]
}

// ------------------------------------------------------------------ loop

pub fn run(paths: Paths) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let (refresh_tx, refresh_rx) = mpsc::channel::<()>();
    {
        let (tx, paths) = (tx.clone(), paths.clone());
        std::thread::spawn(move || loop {
            if tx.send(Msg::Model(Box::new(Model::load(&paths)))).is_err() {
                return;
            }
            let _ = refresh_rx.recv_timeout(REFRESH);
        });
    }
    let mut terminal = setup()?;
    let mut app = App::new(Model::default());
    let result = event_loop(&mut terminal, &mut app, &tx, &rx, &refresh_tx);
    restore(&mut terminal);
    result
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(std::io::stdout()))?)
}

fn restore<B: Backend + std::io::Write>(terminal: &mut Terminal<B>) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    tx: &mpsc::Sender<Msg>,
    rx: &mpsc::Receiver<Msg>,
    refresh: &mpsc::Sender<()>,
) -> Result<()> {
    while !app.quit {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Model(m) => app.model = *m,
                Msg::Done { label, ok, output } => {
                    app.busy = None;
                    let detail = output.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                    let text = if ok { format!("{label}: done {detail}") } else { format!("{label} failed: {detail}") };
                    app.message = Some((text, !ok, Instant::now()));
                    let _ = refresh.send(());
                }
            }
        }
        if app.message.as_ref().is_some_and(|(_, _, at)| at.elapsed() > Duration::from_secs(8)) {
            app.message = None;
        }
        terminal.draw(|f| app.draw(f))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        let Some(action) = app.key(key) else { continue };
        if action.interactive {
            run_in_terminal(terminal, &action)?;
            let _ = refresh.send(());
            continue;
        }
        app.busy = Some(action.command_line());
        let tx = tx.clone();
        std::thread::spawn(move || {
            let (ok, output) = match action.command().output() {
                Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
                Err(e) => (false, e.to_string()),
            };
            let _ = tx.send(Msg::Done { label: action.label.clone(), ok, output });
        });
    }
    Ok(())
}

/// Leaves the TUI, runs the command with the terminal (Ctrl-C stops it, not
/// the TUI), then comes back.
fn run_in_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>, action: &Action) -> Result<()> {
    restore(terminal);
    println!("$ {}   (Ctrl-C to return)", action.command_line());
    let mut cmd = action.command();
    cmd.stdin(Stdio::inherit());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        });
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    let _ = cmd.status();
    unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;
    use ratatui::backend::TestBackend;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn app() -> App {
        let (containers, ports, projects) = crate::actions::tests::sample();
        let status = serde_json::json!({"running": true, "cpus": 6, "memory_mib": 8192, "dockerd": "29.9.0", "fs_cache": true,
            "disk": {"used_in_vm": 1u64 << 30, "size": 10u64 << 30, "on_mac": 2u64 << 30}, "fs_requests": {"1": 5}});
        App::new(Model { running: true, status, containers, ports, projects, sandbox: None })
    }

    fn screen(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
        t.draw(|f| app.draw(f)).unwrap();
        t.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn renders_tabs_and_hints_from_actions() {
        let mut a = app();
        let s = screen(&mut a);
        assert!(s.contains("● rekcod running · 6 CPUs"), "{s}");
        assert!(s.contains("web") && s.contains("nginx"), "{s}");
        assert!(s.contains("x Stop  r Restart  l Logs  d Remove"), "{s}");
        a.key(key(KeyCode::Char('3')));
        let s = screen(&mut a);
        assert!(s.contains("shop") && s.contains("seeded"), "{s}");
    }

    #[test]
    fn keys_pick_actions_and_confirm_destructive_ones() {
        let mut a = app();
        let run = a.key(key(KeyCode::Char('r'))).expect("restart runs at once");
        assert_eq!(run.run, crate::actions::Run::Rekcod(vec!["restart".into(), "web".into()]));
        assert!(a.key(key(KeyCode::Char('d'))).is_none(), "remove asks first");
        assert!(screen(&mut a).contains("Remove container web?"));
        let run = a.key(key(KeyCode::Char('y'))).expect("confirmed");
        assert_eq!(run.run, crate::actions::Run::Rekcod(vec!["rm".into(), "-f".into(), "web".into()]));
        a.key(key(KeyCode::Char('v')));
        assert!(screen(&mut a).contains("Root shell in the VM"));
        assert!(a.key(key(KeyCode::Esc)).is_none());
        a.key(key(KeyCode::Char('q')));
        assert!(a.quit);
    }
}
