//! evalfinder's GPUI app: log in → choose project, count and time window → watch it book.
//!
//! The screens are `evalfinder-app`'s; this file only draws them. Backend work (HTTP, D-Bus,
//! Chrome) runs on a tokio runtime owned by the app; results come back through GPUI tasks.

use std::{future::Future, sync::Arc};

use evalfinder_app::{
    Backend, JobUpdate, LoginForm, RunView, SetupForm,
    backend::Canceller,
    model::{local_day, local_hm, local_hms},
};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Root, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    progress::Progress,
    radio::Radio,
    v_flex,
};
use jiff::Timestamp;
use megaft_core::Project;

#[derive(Debug)]
enum Screen {
    Connecting,
    Login(LoginForm),
    Setup(SetupForm),
    Run {
        view: RunView,
        cancel: Option<Canceller>,
    },
}

struct EvalFinder {
    rt: tokio::runtime::Handle,
    backend: Option<Arc<dyn Backend>>,
    screen: Screen,
    who: String,
    login: Entity<InputState>,
    password: Entity<InputState>,
    count: Entity<InputState>,
    hours: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EvalFinder {
    fn new(
        rt: tokio::runtime::Handle,
        force_local: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let login = cx.new(|cx| InputState::new(window, cx).placeholder("intra login"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("password").masked(true));
        let count = cx.new(|cx| InputState::new(window, cx).default_value("1"));
        let hours = cx.new(|cx| InputState::new(window, cx).default_value("4"));
        let subs = vec![
            cx.subscribe_in(&login, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.password.update(cx, |p, cx| p.focus(window, cx));
                }
            }),
            cx.subscribe_in(&password, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.do_login(window, cx);
                }
            }),
            cx.subscribe_in(&hours, window, |_, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify(); // refresh the deadline hint
                }
            }),
        ];
        let this = Self {
            rt,
            backend: None,
            screen: Screen::Connecting,
            who: String::new(),
            login,
            password,
            count,
            hours,
            _subscriptions: subs,
        };
        this.background(
            window,
            cx,
            evalfinder_app::connect(force_local),
            |this, backend, _, cx| {
                this.backend = Some(backend);
                this.screen = Screen::Login(LoginForm::default());
                cx.notify();
            },
        );
        this
    }

    /// Runs `fut` on the tokio runtime, then `then` on the UI thread with its output.
    fn background<T, F, G>(&self, window: &Window, cx: &mut Context<'_, Self>, fut: F, then: G)
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
        G: FnOnce(&mut Self, T, &mut Window, &mut Context<'_, Self>) + 'static,
    {
        let task = self.rt.spawn(fut);
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(out) => {
                if let Err(e) = this.update_in(cx, |this, window, cx| then(this, out, window, cx)) {
                    tracing::debug!("view gone: {e}");
                }
            }
            Err(e) => tracing::error!("background task failed: {e}"),
        })
        .detach();
    }

    fn backend(&self) -> Option<Arc<dyn Backend>> {
        self.backend.clone()
    }

    // ── login ──

    fn login_result(&mut self, r: Result<String, String>, window: &mut Window, cx: &mut Context<'_, Self>) {
        match r {
            Ok(who) => {
                window.push_notification(Notification::success(format!("Logged in as {who}")), cx);
                self.goto_setup(who, window, cx);
            }
            Err(e) => {
                if let Screen::Login(form) = &mut self.screen {
                    form.busy = None;
                    form.error = Some(e);
                }
            }
        }
        cx.notify();
    }

    fn set_busy(&mut self, msg: &str, cx: &mut Context<'_, Self>) {
        if let Screen::Login(form) = &mut self.screen {
            form.error = None;
            form.busy = Some(msg.to_owned());
        }
        cx.notify();
    }

    fn do_login(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(backend) = self.backend() else { return };
        let Screen::Login(form) = &mut self.screen else {
            return;
        };
        if form.busy.is_some() {
            return;
        }
        form.login = self.login.read(cx).value().to_string();
        form.password = self.password.read(cx).value().to_string();
        let creds = form.credentials();
        form.clear_password();
        self.password.update(cx, |p, cx| p.set_value("", window, cx)); // don't keep it in the field
        match creds {
            Ok((login, password)) => {
                self.set_busy("Logging in with headless Chrome… (up to a minute)", cx);
                self.background(
                    window,
                    cx,
                    async move { backend.login(login, password).await.map_err(|e| e.to_string()) },
                    Self::login_result,
                );
            }
            Err(e) => {
                form.error = Some(e);
                cx.notify();
            }
        }
    }

    fn do_saved_session(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(backend) = self.backend() else { return };
        self.set_busy("Checking the saved session…", cx);
        self.background(
            window,
            cx,
            async move { backend.use_saved_session().await.map_err(|e| e.to_string()) },
            Self::login_result,
        );
    }

    fn do_browser_login(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(backend) = self.backend() else { return };
        self.set_busy("Log in in the Chrome window that opens (5 min max).", cx);
        self.background(
            window,
            cx,
            async move { backend.login_interactive().await.map_err(|e| e.to_string()) },
            Self::login_result,
        );
    }

    // ── setup ──

    fn goto_setup(&mut self, who: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(backend) = self.backend() else { return };
        self.who.clone_from(&who);
        self.screen = Screen::Setup(SetupForm::new(who, backend.describe()));
        self.background(
            window,
            cx,
            async move { backend.open_projects().await.map_err(|e| e.to_string()) },
            |this, r: Result<Vec<Project>, String>, _, cx| {
                if let Screen::Setup(form) = &mut this.screen {
                    match r {
                        Ok(p) => form.set_projects(p),
                        Err(e) => {
                            form.loading = false;
                            form.error = Some(format!("Couldn't load projects: {e}"));
                        }
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    fn do_start(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(backend) = self.backend() else { return };
        let Screen::Setup(form) = &mut self.screen else {
            return;
        };
        form.count = self.count.read(cx).value().to_string();
        form.hours = self.hours.read(cx).value().to_string();
        let spec = match form.job_spec(Timestamp::now()) {
            Ok(s) => s,
            Err(e) => {
                form.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.screen = Screen::Run {
            view: RunView::new(spec.clone()),
            cancel: None,
        };
        cx.notify();
        let task = self
            .rt
            .spawn(async move { backend.start_job(spec).await.map_err(|e| e.to_string()) });
        cx.spawn_in(window, async move |this, cx| {
            let mut updates = match task.await {
                Ok(Ok(handle)) => {
                    let cancel = handle.canceller();
                    let set = this.update(cx, |this, cx| {
                        if let Screen::Run { cancel: slot, .. } = &mut this.screen {
                            *slot = Some(cancel);
                        }
                        cx.notify();
                    });
                    if set.is_err() {
                        return;
                    }
                    handle.updates
                }
                Ok(Err(e)) => {
                    let finished = JobUpdate::Finished {
                        state: "error".into(),
                        summary: format!("Error: {e}"),
                    };
                    if let Err(e) =
                        this.update_in(cx, |this, window, cx| this.on_update(finished, window, cx))
                    {
                        tracing::debug!("view gone: {e}");
                    }
                    return;
                }
                Err(e) => {
                    tracing::error!("start_job task: {e}");
                    return;
                }
            };
            // tokio's mpsc works from any executor, so the UI can await it directly.
            while let Some(u) = updates.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_update(u, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_update(&mut self, u: JobUpdate, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Screen::Run { view, .. } = &mut self.screen
            && let Some(b) = view.apply(u)
        {
            let verb = if b.dry_run { "Would book" } else { "Booked" };
            let msg = format!(
                "{verb} {} {}–{}",
                local_day(b.start),
                local_hm(b.start),
                local_hm(b.end)
            );
            window.push_notification(Notification::success(msg), cx);
        }
        cx.notify();
    }

    fn do_stop(&mut self, cx: &mut Context<'_, Self>) {
        if let Screen::Run {
            view,
            cancel: Some(c),
        } = &mut self.screen
            && view.is_running()
        {
            view.stopping = true;
            view.status = "Stopping…".into();
            let c = c.clone();
            self.rt.spawn(async move {
                if let Err(e) = c.cancel().await {
                    tracing::warn!("cancel: {e}");
                }
            });
            cx.notify();
        }
    }

    // ── drawing ──

    fn card(&self, cx: &Context<'_, Self>) -> gpui::Div {
        v_flex()
            .w(px(520.))
            .p_6()
            .gap_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .bg(cx.theme().background)
    }

    fn hint(text: impl Into<SharedString>, cx: &Context<'_, Self>) -> gpui::Div {
        div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
    }

    fn render_login(&self, form: &LoginForm, cx: &mut Context<'_, Self>) -> AnyElement {
        let busy = form.busy.is_some();
        self.card(cx)
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child("Log in to the 42 intra"),
            )
            .child(Self::hint(
                "Your login and password are kept in memory only and never written to disk.",
                cx,
            ))
            .child(div().pt_2().child("Login"))
            .child(Input::new(&self.login))
            .child(div().pt_2().child("Password"))
            .child(Input::new(&self.password))
            .when_some(form.busy.clone(), |d, b| d.child(Self::hint(b, cx)))
            .when_some(form.error.clone(), |d, e| {
                d.child(div().text_sm().text_color(cx.theme().danger).child(e))
            })
            .child(
                v_flex()
                    .pt_3()
                    .gap_2()
                    .child(
                        Button::new("login")
                            .primary()
                            .w_full()
                            .label("Log in")
                            .loading(busy)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.do_login(window, cx))),
                    )
                    .child(
                        Button::new("session")
                            .outline()
                            .w_full()
                            .label("Continue with current session")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.do_saved_session(window, cx))),
                    )
                    .child(
                        Button::new("browser")
                            .outline()
                            .w_full()
                            .label("Use a browser window")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.do_browser_login(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_setup(&self, form: &SetupForm, cx: &mut Context<'_, Self>) -> AnyElement {
        let hours: SetupForm = SetupForm {
            hours: self.hours.read(cx).value().to_string(),
            ..form.clone()
        };
        let projects = if form.loading {
            v_flex().child(Self::hint("Loading your projects…", cx))
        } else if form.projects.is_empty() {
            v_flex().child(Self::hint("No open projects", cx))
        } else {
            v_flex()
                .gap_1()
                .children(form.projects.iter().enumerate().map(|(i, p)| {
                    Radio::new(("project", i))
                        .label(format!("{}  —  {}", p.name, p.status))
                        .checked(form.selected == Some(i))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Screen::Setup(f) = &mut this.screen {
                                f.selected = Some(i);
                                f.error = None;
                            }
                            cx.notify();
                        }))
                }))
        };
        self.card(cx)
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child("Book evaluations"),
            )
            .child(Self::hint(format!("Logged in as {}", form.who), cx))
            .child(Self::hint(format!("Jobs run in {}", form.backend), cx))
            .child(div().pt_2().child("Project"))
            .child(projects)
            .child(
                h_flex()
                    .pt_2()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child("Number of evaluations")
                            .child(Input::new(&self.count)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child("Within how many hours")
                            .child(Input::new(&self.hours)),
                    ),
            )
            .child(Self::hint(hours.deadline_hint(Timestamp::now()), cx))
            .child(
                div().pt_2().child(
                    Checkbox::new("dry")
                        .label("Dry run (don't actually book)")
                        .checked(form.dry_run)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            if let Screen::Setup(f) = &mut this.screen {
                                f.dry_run = *checked;
                            }
                            cx.notify();
                        })),
                ),
            )
            .when_some(form.error.clone(), |d, e| {
                d.child(div().text_sm().text_color(cx.theme().danger).child(e))
            })
            .child(
                h_flex()
                    .pt_3()
                    .gap_3()
                    .child(
                        Button::new("start")
                            .primary()
                            .flex_1()
                            .label("Start")
                            .disabled(form.loading)
                            .on_click(cx.listener(|this, _, window, cx| this.do_start(window, cx))),
                    )
                    .child(
                        Button::new("relogin")
                            .outline()
                            .flex_1()
                            .label("Change login")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Login(LoginForm {
                                    login: this.who.clone(),
                                    ..LoginForm::default()
                                });
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_run(&self, view: &RunView, cx: &mut Context<'_, Self>) -> AnyElement {
        let done = view
            .booked
            .len()
            .min(usize::try_from(view.spec.count).unwrap_or(usize::MAX));
        let pct = if view.spec.count == 0 {
            0.0
        } else {
            f32::from(u16::try_from(done).unwrap_or(u16::MAX)) * 100.0
                / f32::from(u16::try_from(view.spec.count).unwrap_or(u16::MAX))
        };
        let status_color = match view.finished.as_ref().map(|(s, _)| s.as_str()) {
            Some("done") => cx.theme().success,
            Some("error") => cx.theme().danger,
            Some(_) => cx.theme().warning,
            None => cx.theme().primary,
        };
        let header = h_flex()
            .gap_6()
            .text_sm()
            .font_weight(FontWeight::BOLD)
            .child(div().w(px(24.)).child("#"))
            .child(div().w(px(120.)).child("Day"))
            .child(div().w(px(60.)).child("From"))
            .child(div().w(px(60.)).child("To"));
        let table = v_flex()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if view.spec.dry_run {
                        "Found (dry run)"
                    } else {
                        "Booked"
                    }),
            )
            .child(header)
            .children(view.booked.iter().enumerate().map(|(i, b)| {
                h_flex()
                    .gap_6()
                    .text_sm()
                    .child(div().w(px(24.)).child(i.saturating_add(1).to_string()))
                    .child(div().w(px(120.)).child(local_day(b.start)))
                    .child(div().w(px(60.)).child(local_hm(b.start)))
                    .child(div().w(px(60.)).child(local_hm(b.end)))
            }));
        let log = div()
            .id("log")
            .flex_1()
            .min_h(px(120.))
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .overflow_y_scroll()
            .children(view.log.iter().rev().map(|(t, l)| {
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(div().text_color(cx.theme().muted_foreground).child(local_hms(*t)))
                    .child(l.clone())
            }));
        let running = view.is_running();
        v_flex()
            .size_full()
            .p_6()
            .gap_3()
            .child(div().font_weight(FontWeight::BOLD).child(view.title()))
            .child(div().text_color(status_color).child(view.status.clone()))
            .child(
                h_flex()
                    .gap_3()
                    .child(Progress::new("progress").value(pct).flex_1())
                    .child(format!("{done}/{}", view.spec.count)),
            )
            .when(!view.booked.is_empty(), |d| d.child(table))
            .child(log)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("stop")
                            .danger()
                            .label(if view.stopping { "Stopping…" } else { "Stop" })
                            .disabled(!running || view.stopping)
                            .on_click(cx.listener(|this, _, _, cx| this.do_stop(cx))),
                    )
                    .child(
                        Button::new("back")
                            .outline()
                            .label("Back")
                            .disabled(running)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let who = this.who.clone();
                                this.goto_setup(who, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for EvalFinder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let body = match &self.screen {
            Screen::Connecting => Self::hint("Connecting…", cx).into_any_element(),
            Screen::Login(form) => {
                let form = form.clone();
                self.render_login(&form, cx)
            }
            Screen::Setup(form) => {
                let form = form.clone();
                self.render_setup(&form, cx)
            }
            Screen::Run { view, .. } => {
                let view = view.clone();
                self.render_run(&view, cx)
            }
        };
        let centered = !matches!(self.screen, Screen::Run { .. });
        // Notifications are drawn by gpui-component's Root (a root plugin), not here.
        div()
            .id("root")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(centered, |d| {
                d.flex().justify_center().items_start().pt_8().overflow_y_scroll()
            })
            .child(body)
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let force_local = std::env::args().any(|a| a == "--local");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let handle = runtime.handle().clone();
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = gpui::Bounds::centered(None, size(px(720.), px(820.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("evalfinder".into()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            window.set_window_title("evalfinder");
            let view = cx.new(|cx| EvalFinder::new(handle.clone(), force_local, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        });
        if let Err(err) = opened {
            tracing::error!("could not open window: {err:#}");
            cx.quit();
        }
    });
    drop(runtime);
    Ok(())
}
