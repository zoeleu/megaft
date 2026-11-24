//! The ratatui front-end: login → setup → run, same as the GPUI app (both draw `evalfinder-app`).

use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use evalfinder_app::{
    Backend, JobUpdate, LoginForm, RunView, SetupForm,
    backend::Canceller,
    model::{local_day, local_hm, local_hms},
};
use futures::StreamExt as _;
use jiff::Timestamp;
use megaft_core::Project;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize as _},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
    },
};
use tokio::sync::mpsc;

// ───────────────────────────── text field ─────────────────────────────

/// A one-line text input: cursor movement, insert, delete.
#[derive(Debug, Default, Clone)]
struct TextField {
    text: String,
    /// Cursor position in chars.
    cursor: usize,
}

impl TextField {
    fn with(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            cursor: text.chars().count(),
        }
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Returns `true` if the key was a text-editing key.
    fn handle(&mut self, key: KeyEvent) -> bool {
        let len = self.text.chars().count();
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = self.byte_at(self.cursor);
                self.text.insert(at, c);
                self.cursor = self.cursor.saturating_add(1);
            }
            KeyCode::Char('u') => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor = self.cursor.saturating_sub(1);
                let at = self.byte_at(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte_at(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = self.cursor.saturating_add(1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }

    fn wipe(&mut self) {
        let len = self.text.len();
        self.text.clear();
        self.text.extend(std::iter::repeat_n('\0', len));
        self.text.clear();
        self.cursor = 0;
    }
}

// ───────────────────────────── app state ─────────────────────────────

/// Results from background tasks.
#[derive(Debug)]
enum Msg {
    LoggedIn(Result<String, String>),
    Projects(Result<Vec<Project>, String>),
    JobStarted(Result<(mpsc::UnboundedReceiver<JobUpdate>, Canceller), String>),
    Job(JobUpdate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginFocus {
    Login,
    Password,
    LogIn,
    Session,
    Browser,
}

impl LoginFocus {
    const ORDER: [Self; 5] = [
        Self::Login,
        Self::Password,
        Self::LogIn,
        Self::Session,
        Self::Browser,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupFocus {
    Project,
    Count,
    Hours,
    DryRun,
    Start,
    Relogin,
}

impl SetupFocus {
    const ORDER: [Self; 6] = [
        Self::Project,
        Self::Count,
        Self::Hours,
        Self::DryRun,
        Self::Start,
        Self::Relogin,
    ];
}

fn cycle<T: Copy + PartialEq>(order: &[T], current: T, back: bool) -> T {
    let i = order.iter().position(|f| *f == current).unwrap_or(0);
    let n = order.len();
    let next = if back {
        i.checked_sub(1).unwrap_or(n.saturating_sub(1))
    } else {
        (i.saturating_add(1)) % n.max(1)
    };
    order.get(next).copied().unwrap_or(current)
}

#[derive(Debug)]
enum Screen {
    Login {
        form: LoginForm,
        login: TextField,
        password: TextField,
        focus: LoginFocus,
    },
    Setup {
        form: SetupForm,
        count: TextField,
        hours: TextField,
        focus: SetupFocus,
        list: ListState,
    },
    Run {
        view: RunView,
        cancel: Option<Canceller>,
    },
}

#[derive(Debug)]
struct App {
    backend: Arc<dyn Backend>,
    tx: mpsc::UnboundedSender<Msg>,
    screen: Screen,
    who: String,
    quit: bool,
    notice: Option<String>,
}

impl App {
    fn login_screen(login: &str) -> Screen {
        Screen::Login {
            form: LoginForm {
                login: login.to_owned(),
                ..LoginForm::default()
            },
            login: TextField::with(login),
            password: TextField::default(),
            focus: if login.is_empty() {
                LoginFocus::Login
            } else {
                LoginFocus::Password
            },
        }
    }

    fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = Msg> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if tx.send(fut.await).is_err() {
                tracing::debug!("UI closed before a task finished");
            }
        });
    }

    fn goto_setup(&mut self, who: String) {
        self.who.clone_from(&who);
        let form = SetupForm::new(who, self.backend.describe());
        let (count, hours) = (TextField::with(&form.count), TextField::with(&form.hours));
        self.screen = Screen::Setup {
            form,
            count,
            hours,
            focus: SetupFocus::Project,
            list: ListState::default(),
        };
        let backend = self.backend.clone();
        self.spawn(async move { Msg::Projects(backend.open_projects().await.map_err(|e| e.to_string())) });
    }

    fn on_msg(&mut self, msg: Msg) {
        match (msg, &mut self.screen) {
            (Msg::LoggedIn(Ok(who)), Screen::Login { password, .. }) => {
                password.wipe();
                self.goto_setup(who);
            }
            (Msg::LoggedIn(Err(e)), Screen::Login { form, .. }) => {
                form.busy = None;
                form.error = Some(e);
            }
            (Msg::Projects(r), Screen::Setup { form, list, .. }) => match r {
                Ok(p) => {
                    form.set_projects(p);
                    list.select(form.selected);
                }
                Err(e) => {
                    form.loading = false;
                    form.error = Some(format!("Couldn't load projects: {e}"));
                }
            },
            (Msg::JobStarted(Ok((mut rx, cancel))), Screen::Run { cancel: slot, .. }) => {
                *slot = Some(cancel);
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    while let Some(u) = rx.recv().await {
                        if tx.send(Msg::Job(u)).is_err() {
                            break;
                        }
                    }
                });
            }
            (Msg::JobStarted(Err(e)), Screen::Run { view, .. }) => {
                view.apply(JobUpdate::Finished {
                    state: "error".into(),
                    summary: format!("Error: {e}"),
                });
            }
            (Msg::Job(u), Screen::Run { view, .. }) => {
                if let Some(b) = view.apply(u) {
                    let verb = if b.dry_run { "Would book" } else { "Booked" };
                    self.notice = Some(format!(
                        "{verb} {} {}–{}",
                        local_day(b.start),
                        local_hm(b.start),
                        local_hm(b.end)
                    ));
                    print!("\x07"); // terminal bell
                }
            }
            (msg, _) => tracing::debug!("ignored {msg:?} on this screen"),
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        let back = key.code == KeyCode::BackTab;
        let backend = self.backend.clone();
        match &mut self.screen {
            Screen::Login {
                form,
                login,
                password,
                focus,
            } => {
                if form.busy.is_some() {
                    return;
                }
                match key.code {
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                        let back = back || key.code == KeyCode::Up;
                        *focus = cycle(&LoginFocus::ORDER, *focus, back);
                    }
                    KeyCode::Esc => self.quit = true,
                    KeyCode::Enter => match focus {
                        LoginFocus::Login => *focus = LoginFocus::Password,
                        LoginFocus::Password | LoginFocus::LogIn => {
                            form.login.clone_from(&login.text);
                            form.password.clone_from(&password.text);
                            match form.credentials() {
                                Ok((user, pw)) => {
                                    form.clear_password();
                                    form.error = None;
                                    form.busy =
                                        Some("Logging in with headless Chrome… (up to a minute)".into());
                                    let b = backend.clone();
                                    self.spawn(async move {
                                        Msg::LoggedIn(b.login(user, pw).await.map_err(|e| e.to_string()))
                                    });
                                }
                                Err(e) => form.error = Some(e),
                            }
                        }
                        LoginFocus::Session => {
                            form.error = None;
                            form.busy = Some("Checking the saved session…".into());
                            let b = backend.clone();
                            self.spawn(async move {
                                Msg::LoggedIn(b.use_saved_session().await.map_err(|e| e.to_string()))
                            });
                        }
                        LoginFocus::Browser => {
                            form.error = None;
                            form.busy = Some("Log in in the Chrome window that opens (5 min max).".into());
                            let b = backend.clone();
                            self.spawn(async move {
                                Msg::LoggedIn(b.login_interactive().await.map_err(|e| e.to_string()))
                            });
                        }
                    },
                    _ => match focus {
                        LoginFocus::Login => {
                            login.handle(key);
                        }
                        LoginFocus::Password => {
                            password.handle(key);
                        }
                        _ => {}
                    },
                }
            }
            Screen::Setup {
                form,
                count,
                hours,
                focus,
                list,
            } => match key.code {
                KeyCode::Tab | KeyCode::BackTab => *focus = cycle(&SetupFocus::ORDER, *focus, back),
                KeyCode::Esc => self.quit = true,
                KeyCode::Up | KeyCode::Down if *focus == SetupFocus::Project => {
                    let n = form.projects.len();
                    if n > 0 {
                        let i = form.selected.unwrap_or(0);
                        let i = if key.code == KeyCode::Up {
                            i.checked_sub(1).unwrap_or(n - 1)
                        } else {
                            (i + 1) % n
                        };
                        form.selected = Some(i);
                        list.select(Some(i));
                    }
                }
                KeyCode::Up => *focus = cycle(&SetupFocus::ORDER, *focus, true),
                KeyCode::Down => *focus = cycle(&SetupFocus::ORDER, *focus, false),
                KeyCode::Char(' ') if *focus == SetupFocus::DryRun => form.dry_run = !form.dry_run,
                KeyCode::Enter => match focus {
                    SetupFocus::DryRun => form.dry_run = !form.dry_run,
                    SetupFocus::Relogin => self.screen = Self::login_screen(&self.who),
                    SetupFocus::Project | SetupFocus::Count | SetupFocus::Hours | SetupFocus::Start => {
                        form.count.clone_from(&count.text);
                        form.hours.clone_from(&hours.text);
                        match form.job_spec(Timestamp::now()) {
                            Ok(spec) => {
                                self.screen = Screen::Run {
                                    view: RunView::new(spec.clone()),
                                    cancel: None,
                                };
                                self.spawn(async move {
                                    Msg::JobStarted(
                                        backend
                                            .start_job(spec)
                                            .await
                                            .map(|h| {
                                                let c = h.canceller();
                                                (h.updates, c)
                                            })
                                            .map_err(|e| e.to_string()),
                                    )
                                });
                            }
                            Err(e) => form.error = Some(e),
                        }
                    }
                },
                _ => {
                    let edited = match focus {
                        SetupFocus::Count => count.handle(key),
                        SetupFocus::Hours => hours.handle(key),
                        _ => false,
                    };
                    if edited {
                        form.count.clone_from(&count.text);
                        form.hours.clone_from(&hours.text);
                        form.error = None;
                    } else if key.code == KeyCode::Char('q') {
                        self.quit = true;
                    }
                }
            },
            Screen::Run { view, cancel } => match key.code {
                KeyCode::Char('s') | KeyCode::Esc if view.is_running() => {
                    if let Some(c) = cancel.clone() {
                        view.stopping = true;
                        view.status = "Stopping…".into();
                        tokio::spawn(async move {
                            if let Err(e) = c.cancel().await {
                                tracing::warn!("cancel: {e}");
                            }
                        });
                    }
                }
                KeyCode::Char('b') | KeyCode::Esc | KeyCode::Enter if !view.is_running() => {
                    let who = self.who.clone();
                    self.goto_setup(who);
                }
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
        }
    }
}

// ───────────────────────────── drawing ─────────────────────────────

const ACCENT: Color = Color::Cyan;

fn card(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x.saturating_add(area.width.saturating_sub(w) / 2);
    let y = area.y.saturating_add(area.height.saturating_sub(h) / 3);
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn field<'a>(label: &'a str, value: String, focused: bool) -> Paragraph<'a> {
    let border = if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Paragraph::new(value).block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(label),
    )
}

fn button(label: &str, focused: bool, primary: bool) -> Paragraph<'_> {
    let style = match (focused, primary) {
        (true, _) => Style::default()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
        (false, true) => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        (false, false) => Style::default(),
    };
    Paragraph::new(Line::from(format!(" {label} ")).centered()).style(style)
}

fn set_cursor(frame: &mut Frame<'_>, area: Rect, f: &TextField) {
    let col = u16::try_from(f.cursor).unwrap_or(u16::MAX);
    frame.set_cursor_position((
        area.x.saturating_add(1).saturating_add(col),
        area.y.saturating_add(1),
    ));
}

fn message(form_error: Option<&String>, busy: Option<&String>) -> Option<Paragraph<'static>> {
    if let Some(b) = busy {
        return Some(
            Paragraph::new(b.clone())
                .fg(Color::Yellow)
                .wrap(Wrap { trim: true }),
        );
    }
    form_error.map(|e| Paragraph::new(e.clone()).fg(Color::Red).wrap(Wrap { trim: true }))
}

fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)])
            .areas(frame.area());
    frame.render_widget(
        Line::from(vec![
            " evalfinder ".bold().fg(Color::Black).bg(ACCENT),
            " 42 evaluation slot sniper".into(),
        ]),
        header,
    );
    let help = match &app.screen {
        Screen::Login { .. } => "Tab/↑↓ move · Enter select · Esc quit",
        Screen::Setup { .. } => "Tab move · ↑↓ choose project · Space toggle · Enter start · Esc quit",
        Screen::Run { view, .. } if view.is_running() => "s stop · q quit (a megaftd job keeps running)",
        Screen::Run { .. } => "b back · q quit",
    };
    frame.render_widget(Line::from(help).fg(Color::DarkGray), footer);

    match &mut app.screen {
        Screen::Login {
            form,
            login,
            password,
            focus,
        } => {
            let area = card(body, 64, 22);
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT));
            let inner = block.inner(area).inner(ratatui::layout::Margin {
                horizontal: 2,
                vertical: 1,
            });
            frame.render_widget(block, area);
            let [title, hint, f_login, f_pw, msg, b1, b2, b3] = Layout::vertical([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            frame.render_widget(Line::from("Log in to the 42 intra").bold(), title);
            frame.render_widget(
                Paragraph::new("Your login and password are kept in memory only and never written to disk.")
                    .fg(Color::DarkGray)
                    .wrap(Wrap { trim: true }),
                hint,
            );
            frame.render_widget(
                field("Login", login.text.clone(), *focus == LoginFocus::Login),
                f_login,
            );
            let masked = "•".repeat(password.text.chars().count());
            frame.render_widget(field("Password", masked, *focus == LoginFocus::Password), f_pw);
            if let Some(p) = message(form.error.as_ref(), form.busy.as_ref()) {
                frame.render_widget(p, msg);
            }
            frame.render_widget(button("Log in", *focus == LoginFocus::LogIn, true), b1);
            frame.render_widget(
                button(
                    "Continue with current session",
                    *focus == LoginFocus::Session,
                    false,
                ),
                b2,
            );
            frame.render_widget(
                button("Use a browser window", *focus == LoginFocus::Browser, false),
                b3,
            );
            match focus {
                LoginFocus::Login => set_cursor(frame, f_login, login),
                LoginFocus::Password => set_cursor(frame, f_pw, password),
                _ => {}
            }
        }
        Screen::Setup {
            form,
            count,
            hours,
            focus,
            list,
        } => {
            let rows = u16::try_from(form.projects.len().clamp(1, 6)).unwrap_or(6);
            let area = card(body, 70, 22u16.saturating_add(rows));
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT));
            let inner = block.inner(area).inner(ratatui::layout::Margin {
                horizontal: 2,
                vertical: 1,
            });
            frame.render_widget(block, area);
            let [title, who, projects, nums, hint, dry, msg, buttons] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(rows.saturating_add(2)),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(1),
            ])
            .areas(inner);
            frame.render_widget(Line::from("Book evaluations").bold(), title);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec!["Logged in as ".fg(Color::DarkGray), form.who.clone().bold()]),
                    Line::from(vec!["Jobs run in ".fg(Color::DarkGray), form.backend.into()]),
                ]),
                who,
            );
            let items: Vec<ListItem<'_>> = if form.loading {
                vec![ListItem::new("Loading your projects…".fg(Color::DarkGray))]
            } else if form.projects.is_empty() {
                vec![ListItem::new("No open projects".fg(Color::DarkGray))]
            } else {
                form.projects
                    .iter()
                    .map(|p| {
                        ListItem::new(Line::from(vec![
                            p.name.clone().into(),
                            "  —  ".fg(Color::DarkGray),
                            p.status.clone().into(),
                        ]))
                    })
                    .collect()
            };
            let border = if *focus == SetupFocus::Project {
                Style::default().fg(ACCENT)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::bordered()
                            .border_type(BorderType::Rounded)
                            .border_style(border)
                            .title("Project"),
                    )
                    .highlight_symbol("› ")
                    .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
                projects,
                list,
            );
            let [c_area, h_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
                .spacing(2)
                .areas(nums);
            frame.render_widget(
                field(
                    "Number of evaluations",
                    count.text.clone(),
                    *focus == SetupFocus::Count,
                ),
                c_area,
            );
            frame.render_widget(
                field(
                    "Within how many hours",
                    hours.text.clone(),
                    *focus == SetupFocus::Hours,
                ),
                h_area,
            );
            frame.render_widget(
                Paragraph::new(form.deadline_hint(Timestamp::now())).fg(Color::DarkGray),
                hint,
            );
            let mark = if form.dry_run { "[x]" } else { "[ ]" };
            let style = if *focus == SetupFocus::DryRun {
                Style::default().fg(ACCENT).bold()
            } else {
                Style::default()
            };
            frame.render_widget(
                Line::from(format!("{mark} Dry run (don't actually book)")).style(style),
                dry,
            );
            if let Some(p) = message(form.error.as_ref(), None) {
                frame.render_widget(p, msg);
            }
            let [b1, b2] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
                .spacing(2)
                .areas(buttons);
            frame.render_widget(button("Start", *focus == SetupFocus::Start, true), b1);
            frame.render_widget(button("Change login", *focus == SetupFocus::Relogin, false), b2);
            match focus {
                SetupFocus::Count => set_cursor(frame, c_area, count),
                SetupFocus::Hours => set_cursor(frame, h_area, hours),
                _ => {}
            }
        }
        Screen::Run { view, .. } => {
            let area = body.inner(ratatui::layout::Margin {
                horizontal: 2,
                vertical: 1,
            });
            let booked_rows = u16::try_from(view.booked.len()).unwrap_or(u16::MAX).min(6);
            let [title, status, gauge, _, table, log] = Layout::vertical([
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(if view.booked.is_empty() {
                    0
                } else {
                    booked_rows.saturating_add(3)
                }),
                Constraint::Fill(1),
            ])
            .areas(area);
            frame.render_widget(Line::from(view.title()).bold(), title);
            let status_style = match view.finished.as_ref().map(|(s, _)| s.as_str()) {
                Some("done") => Style::default().fg(Color::Green).bold(),
                Some("error") => Style::default().fg(Color::Red).bold(),
                Some(_) => Style::default().fg(Color::Yellow).bold(),
                None => Style::default().fg(ACCENT),
            };
            frame.render_widget(Paragraph::new(view.status.clone()).style(status_style), status);
            let done = u32::try_from(view.booked.len())
                .unwrap_or(u32::MAX)
                .min(view.spec.count);
            frame.render_widget(
                Gauge::default()
                    .gauge_style(Style::default().fg(Color::Green))
                    .ratio(if view.spec.count == 0 {
                        0.0
                    } else {
                        f64::from(done) / f64::from(view.spec.count)
                    })
                    .label(format!("{done}/{}", view.spec.count)),
                gauge,
            );
            if !view.booked.is_empty() {
                let rows = view.booked.iter().enumerate().map(|(i, b)| {
                    Row::new(vec![
                        Cell::from(i.saturating_add(1).to_string()),
                        Cell::from(local_day(b.start)),
                        Cell::from(local_hm(b.start)),
                        Cell::from(local_hm(b.end)),
                    ])
                });
                frame.render_widget(
                    Table::new(
                        rows,
                        [
                            Constraint::Length(3),
                            Constraint::Length(12),
                            Constraint::Length(6),
                            Constraint::Length(6),
                        ],
                    )
                    .header(Row::new(vec!["#", "Day", "From", "To"]).bold())
                    .block(
                        Block::bordered()
                            .border_type(BorderType::Rounded)
                            .title(if view.spec.dry_run {
                                "Found (dry run)"
                            } else {
                                "Booked"
                            }),
                    ),
                    table,
                );
            }
            let height = usize::from(log.height.saturating_sub(2));
            let lines: Vec<Line<'_>> = view
                .log
                .iter()
                .skip(view.log.len().saturating_sub(height))
                .map(|(t, l)| {
                    Line::from(vec![
                        Span::from(local_hms(*t)).fg(Color::DarkGray),
                        " ".into(),
                        l.clone().into(),
                    ])
                })
                .collect();
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Rounded).title("Log")),
                log,
            );
        }
    }
    if let Some(n) = app.notice.as_ref() {
        let w = u16::try_from(n.chars().count()).unwrap_or(60).saturating_add(4);
        let full = frame.area();
        let area = Rect {
            x: full.width.saturating_sub(w.saturating_add(1)),
            y: full.height.saturating_sub(4),
            width: w,
            height: 3,
        };
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(n.clone())
                .fg(Color::Green)
                .block(Block::bordered().border_type(BorderType::Rounded)),
            area,
        );
    }
}

// ───────────────────────────── loop ─────────────────────────────

/// Runs the TUI until the user quits.
pub async fn run(backend: Arc<dyn Backend>) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, backend).await;
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut DefaultTerminal, backend: Arc<dyn Backend>) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App {
        backend,
        tx,
        screen: App::login_screen(""),
        who: String::new(),
        quit: false,
        notice: None,
    };
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut notice_age = 0u8;
    while !app.quit {
        terminal.draw(|f| draw(f, &mut app))?;
        tokio::select! {
            Some(ev) = events.next() => {
                if let Event::Key(key) = ev? {
                    app.on_key(key);
                }
            }
            Some(msg) = rx.recv() => app.on_msg(msg),
            _ = tick.tick() => {
                // Clears the booking toast after a few seconds; also refreshes the deadline hint.
                if app.notice.is_some() {
                    notice_age = notice_age.saturating_add(1);
                    if notice_age > 6 {
                        app.notice = None;
                        notice_age = 0;
                    }
                }
            }
        }
    }
    if let Screen::Run {
        view,
        cancel: Some(c),
    } = &app.screen
        && view.is_running()
        && app.backend.describe().starts_with("this app")
    {
        c.cancel().await?;
    }
    Ok(())
}
