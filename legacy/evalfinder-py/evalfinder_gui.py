#!/usr/bin/env python3
"""Textual front-end for evalfinder: log in, pick a project, and let it book evaluations.

  .venv/bin/python evalfinder_gui.py
"""
import datetime as dt
import os
import threading

from textual import on, work
from textual.app import App, ComposeResult
from textual.containers import Center, Horizontal, Vertical, VerticalScroll
from textual.screen import Screen
from textual.widgets import (Button, Checkbox, DataTable, Footer, Header, Input, Label, LoadingIndicator,
                             ProgressBar, RichLog, Select, Static)

from rich.markup import escape

import browser_cookies as bc
import evalfinder as ef

COOKIES = os.path.join(ef.HERE, "cookies42.txt")


class Msg(Static):
    """Message line that takes no space while empty."""

    def on_mount(self):
        self.display = False

    def update(self, content=""):
        super().update(content)
        self.display = bool(str(content))


class LoginScreen(Screen):
    BINDINGS = [("escape", "back", "Back")]

    def compose(self) -> ComposeResult:
        yield Header()
        with Center():
            with VerticalScroll(id="card"):
                yield Static("[b]Log in to the 42 intra[/b]", classes="title")
                yield Static("Your login and password are kept in memory only while the app runs and are "
                             "never written to disk.", classes="hint")
                yield Label("Login")
                yield Input(value=(bc.CREDENTIALS or ("", ""))[0], placeholder="intra login", id="login")
                yield Label("Password")
                yield Input(placeholder="password", password=True, id="password")
                yield Msg(id="error")
                yield LoadingIndicator(id="spinner")
                with Vertical(classes="stack"):
                    yield Button("Log in", variant="primary", id="go")
                    yield Button("Continue with current session", id="session",
                                 tooltip="Reuse the intra session saved in cookies42.txt, no password")
                    yield Button("Use a browser window", id="manual",
                                 tooltip="Opens Chrome so you can log in yourself (e.g. with 2FA)")
        yield Footer()

    def on_mount(self):
        self.query_one("#spinner").display = False
        self.query_one("#session").display = os.path.exists(COOKIES)
        self.query_one("#login" if not self.query_one("#login", Input).value else "#password").focus()

    def action_back(self):
        if len(self.app.screen_stack) > 2:
            self.app.pop_screen()

    def _busy(self, on: bool, msg=""):
        self.query_one("#spinner").display = on
        for b in self.query(Button):
            b.disabled = on
        self.query_one("#error", Static).update(msg)

    @on(Input.Submitted)
    def _submitted(self, event: Input.Submitted):
        if event.input.id == "login":
            self.query_one("#password").focus()
        else:
            self._login()

    @on(Button.Pressed, "#go")
    def _login(self):
        login = self.query_one("#login", Input).value.strip()
        pw = self.query_one("#password", Input).value
        if not login or not pw:
            self.query_one("#error", Static).update("[red]Enter your login and password.[/red]")
            return
        self._busy(True, "[dim]Logging in with headless Chrome… (up to a minute)[/dim]")
        self._do_login(login, pw, manual=False)

    @on(Button.Pressed, "#manual")
    def _manual(self):
        self._busy(True, "[yellow]Log in in the Chrome window that opens (5 min max).[/yellow]")
        self._do_login(None, None, manual=True)

    @on(Button.Pressed, "#session")
    def _session(self):
        self._busy(True, "[dim]Checking the saved session…[/dim]")
        self._check_session()

    @work(thread=True, exclusive=True)
    def _check_session(self):
        ef.LOG_HOOK = bc.LOG_HOOK = lambda m: None
        try:
            intra = ef.Intra(COOKIES, use_browser=False)
            who = intra.whoami()
        except Exception:
            who = None
        if not who:
            self.app.call_from_thread(self._busy, False, "[red]Session expired, log in with your password.[/red]")
            return
        self.app.call_from_thread(self._done, who)

    @work(thread=True, exclusive=True)
    def _do_login(self, login, pw, manual):
        bc.LOG_HOOK = lambda m: None
        previous = bc.CREDENTIALS
        if login:
            bc.CREDENTIALS = (login, pw)  # memory only
        try:
            who = bc.refresh_cookies(COOKIES, headless=not manual, fresh=not manual, allow_visible=manual)
        except Exception as e:
            bc.CREDENTIALS = previous
            msg = (f"Login refused: {e}" if isinstance(e, bc.BadCredentials)
                   else f"Login failed ({e}). Try 'Use a browser window'.")
            self.app.call_from_thread(self._busy, False, f"[red]{escape(msg)}[/red]")
            return
        if login and who != login:
            bc.CREDENTIALS = previous
            self.app.call_from_thread(self._busy, False, f"[red]Logged in as {who}, not {login}?[/red]")
            return
        self.app.call_from_thread(self._done, who)

    def _done(self, who):
        self._busy(False)
        self.app.who = who
        self.app.notify(f"Logged in as {who}")
        while len(self.app.screen_stack) > 2:
            self.app.pop_screen()
        self.app.switch_screen(SetupScreen())


class SetupScreen(Screen):
    BINDINGS = [("l", "relogin", "Change login"), ("r", "reload", "Reload projects")]

    def compose(self) -> ComposeResult:
        yield Header()
        with Center():
            with VerticalScroll(id="card"):
                yield Static("[b]Book evaluations[/b]", classes="title")
                yield Static("", id="who", classes="hint")
                yield Label("Project")
                yield Select([], prompt="Loading your projects…", id="project")
                yield Label("Number of evaluations")
                yield Input(value="1", type="integer", id="count")
                yield Label("Within how many hours from now")
                yield Input(value="4", type="number", id="hours")
                yield Static("", id="deadline", classes="hint")
                yield Checkbox("Dry run (don't actually book)", id="dry")
                yield Msg(id="error")
                with Horizontal(classes="buttons"):
                    yield Button("Start", variant="success", id="start")
                    yield Button("Change login", id="relogin")
        yield Footer()

    def on_mount(self):
        self.who = self.app.who or "?"
        self.query_one("#who", Static).update(f"Logged in as [b]{self.who}[/b]")
        self._update_deadline()
        self.set_interval(30, self._update_deadline)
        self.action_reload()

    def action_relogin(self):
        self.app.push_screen(LoginScreen())

    @on(Button.Pressed, "#relogin")
    def _relogin(self):
        self.action_relogin()

    def action_reload(self):
        self._load_projects()

    @work(thread=True, exclusive=True)
    def _load_projects(self):
        ef.LOG_HOOK = bc.LOG_HOOK = lambda m: None
        try:
            intra = ef.Intra(COOKIES)
            projects = intra.open_projects()
        except Exception as e:
            self.app.call_from_thread(self._error, f"Couldn't load projects: {e}")
            return
        self.app.intra = intra
        self.app.call_from_thread(self._set_projects, projects)

    def _set_projects(self, projects):
        sel = self.query_one("#project", Select)
        opts = [(f"{p['name']}  —  {p['status']}", p["slug"]) for p in projects]
        sel.set_options(opts)
        sel.prompt = "Choose a project" if opts else "No open projects"
        wanted = [p["slug"] for p in projects if "evaluation" in p["status"].lower()]
        if wanted:
            sel.value = wanted[0]

    def _error(self, msg):
        self.query_one("#error", Static).update(f"[red]{msg}[/red]")

    @on(Input.Changed, "#hours")
    def _update_deadline(self, *_):
        try:
            h = float(self.query_one("#hours", Input).value)
            d = dt.datetime.now().astimezone() + dt.timedelta(hours=h)
            self.query_one("#deadline", Static).update(f"Evaluations must start before [b]{d:%a %d %b %H:%M}[/b]")
        except ValueError:
            self.query_one("#deadline", Static).update("")

    @on(Button.Pressed, "#start")
    def _start(self):
        project = self.query_one("#project", Select).value
        try:
            count = int(self.query_one("#count", Input).value)
            hours = float(self.query_one("#hours", Input).value)
        except ValueError:
            return self._error("Number of evaluations and hours must be numbers.")
        if project is Select.BLANK or not project:
            return self._error("Choose a project.")
        if count < 1 or hours <= 0:
            return self._error("Need at least 1 evaluation and a positive number of hours.")
        if not getattr(self.app, "intra", None):
            return self._error("Still connecting to the intra…")
        self._error("")
        self.app.push_screen(RunScreen(project, count, hours, self.query_one("#dry", Checkbox).value))


class RunScreen(Screen):
    BINDINGS = [("s", "stop", "Stop"), ("escape", "back", "Back")]

    def __init__(self, project, count, hours, dry):
        super().__init__()
        self.project, self.count, self.dry = project, count, dry
        self.before = dt.datetime.now().astimezone() + dt.timedelta(hours=hours)
        self.stop = threading.Event()
        self.running = False

    def compose(self) -> ComposeResult:
        yield Header()
        with Vertical(id="run"):
            yield Static(f"[b]{self.project}[/b]: {self.count} evaluation(s) starting before "
                         f"[b]{self.before:%a %d %b %H:%M}[/b]" + ("  [yellow](dry run)[/yellow]" if self.dry else ""),
                         id="summary")
            yield Static("Starting…", id="status")
            yield ProgressBar(total=self.count, show_eta=False, id="progress")
            yield DataTable(id="booked", cursor_type="none")
            yield RichLog(id="log", markup=True, wrap=True)
            with Horizontal(classes="buttons"):
                yield Button("Stop", variant="error", id="stop")
                yield Button("Back", id="back", disabled=True)
        yield Footer()

    def on_mount(self):
        t = self.query_one("#booked", DataTable)
        t.add_columns("#", "Day", "From", "To")
        t.display = False
        self.query_one("#progress", ProgressBar).update(progress=0)
        self._run()

    def _log(self, msg, markup=False):
        if not markup:
            msg = escape(msg)  # backend messages contain literal [brackets]
        self.query_one("#log", RichLog).write(f"[dim]{dt.datetime.now():%H:%M:%S}[/dim] {msg}")
        if not msg.startswith("[browser]"):
            self.query_one("#status", Static).update(msg)

    def _booked(self, start, end):
        t = self.query_one("#booked", DataTable)
        t.display = True
        t.add_row(str(t.row_count + 1), f"{start:%a %d %b}", f"{start:%H:%M}", f"{end:%H:%M}")
        self.query_one("#progress", ProgressBar).advance(1)
        self.app.bell()
        self.app.notify(f"{'Would book' if self.dry else 'Booked'} {start:%a %H:%M}-{end:%H:%M}", timeout=10)

    @work(thread=True, exclusive=True)
    def _run(self):
        self.running = True
        hook = lambda m: self.app.call_from_thread(self._log, m)
        ef.LOG_HOOK = bc.LOG_HOOK = hook
        try:
            booked = ef.book_evaluations(
                self.app.intra, self.project, self.before, count=self.count, dry_run=self.dry, stop=self.stop,
                on_booked=lambda s, e: self.app.call_from_thread(self._booked, s, e))
            n = len(booked)
            if self.stop.is_set():
                msg = f"[yellow]Stopped. {n}/{self.count} booked.[/yellow]"
            elif n == self.count:
                msg = f"[green b]Done: {n}/{self.count} {'found (dry run)' if self.dry else 'booked'}.[/green b]"
            else:
                msg = f"[yellow]Deadline reached: {n}/{self.count} booked.[/yellow]"
        except Exception as e:
            msg = f"[red b]Error: {e}[/red b]"
        self.app.call_from_thread(self._finished, msg)

    def _finished(self, msg):
        self.running = False
        self._log(msg, markup=True)
        self.query_one("#stop", Button).disabled = True
        self.query_one("#back", Button).disabled = False

    @on(Button.Pressed, "#stop")
    def action_stop(self):
        if self.running:
            self.stop.set()
            self.query_one("#status", Static).update("Stopping…")

    @on(Button.Pressed, "#back")
    def action_back(self):
        if not self.running:
            self.app.pop_screen()


class EvalFinder(App):
    TITLE = "evalfinder"
    SUB_TITLE = "42 evaluation slot sniper"
    CSS = """
    #card { width: 64; height: auto; max-height: 100%; border: round $primary; padding: 1 2; margin-top: 1; }
    /* Inputs draw an inset border one cell wide: indent text and buttons by the same cell so
       every left/right edge in the card lines up. */
    #card Static, #card Label { padding: 0 1; }
    .title { margin-bottom: 1; }
    .hint { color: $text-muted; margin-bottom: 1; }
    Label { margin-top: 1; }
    #error { margin-top: 1; height: auto; }
    #spinner { height: 1; }
    .stack { height: auto; margin: 1 1 0 1; }
    .stack Button { width: 100%; margin-top: 1; }
    .stack Button:first-child { margin-top: 0; }
    .buttons { height: auto; margin-top: 1; }
    .buttons Button { margin-right: 2; }
    #card .buttons { margin: 1 1 0 1; }
    #card .buttons Button { width: 1fr; margin: 0; }
    #card .buttons Button:first-child { margin-right: 2; }
    #run { padding: 1 2; }
    #summary { margin-bottom: 1; }
    #status { color: $accent; margin-bottom: 1; height: auto; }
    #progress { margin-bottom: 1; }
    #booked { height: auto; max-height: 8; margin-bottom: 1; }
    #log { height: 1fr; border: round $panel; }
    """

    def on_mount(self):
        self.intra = None
        self.who = None
        self.push_screen(LoginScreen())


if __name__ == "__main__":
    EvalFinder().run()
