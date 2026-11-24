//! `evalfinder`: book 42 evaluations from the terminal (TUI, or headless subcommands).

mod tui;

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use jiff::{SignedDuration, Timestamp, tz::TimeZone};
use megaft_core::{IntraWeb, JobEvent, JobSpec, NoRefresh, Session};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "evalfinder", version, about)]
struct Cli {
    /// Run jobs in this process even if megaftd is installed.
    #[arg(long, global = true)]
    local: bool,
    /// Without a subcommand, opens the TUI.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Poll for slots and book evaluations (headless).
    Book(BookArgs),
    /// Import cookies from a Netscape cookies.txt (e.g. the Python evalfinder's cookies42.txt).
    ImportCookies { path: PathBuf },
    /// Show who the saved session belongs to and the projects with an open team.
    Status,
    /// Log in with Chrome and save the session (headless with the saved session, or a visible window).
    Login {
        /// Show the Chrome window (log in yourself, e.g. with 2FA).
        #[arg(long)]
        visible: bool,
        /// Ignore the saved session and log in with a password (asked on the terminal).
        #[arg(long)]
        fresh: bool,
        /// Intra login for the password login.
        #[arg(long)]
        user: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
struct BookArgs {
    /// Project slug, e.g. `codexion`.
    project: String,
    /// Evaluations must start within this many hours from now.
    #[arg(long, conflicts_with = "before")]
    hours: Option<f64>,
    /// Evaluations must start before this local time (`HH:MM`, next occurrence) or ISO timestamp.
    #[arg(long)]
    before: Option<String>,
    /// How many evaluations to book.
    #[arg(long, default_value_t = 1)]
    count: u32,
    /// Minutes kept clear around other evaluations.
    #[arg(long, default_value_t = 15)]
    buffer: u32,
    /// Minimum minutes from now before a slot starts.
    #[arg(long, default_value_t = 0)]
    lead: u32,
    /// Seconds between polls.
    #[arg(long, default_value_t = 15)]
    interval: u32,
    /// Show what would be booked, don't book.
    #[arg(long)]
    dry_run: bool,
}

fn deadline(args: &BookArgs) -> anyhow::Result<Timestamp> {
    let now = jiff::ZonedDateTime::now();
    if let Some(h) = args.hours {
        if !h.is_finite() || h <= 0.0 {
            bail!("--hours must be positive");
        }
        let secs = SignedDuration::try_from_secs_f64(h * 3600.0).context("--hours is too large")?;
        return Ok(now.timestamp() + secs);
    }
    let Some(b) = &args.before else {
        bail!("give --hours or --before")
    };
    if let Some((h, m)) = b.split_once(':')
        && let (Ok(h), Ok(m)) = (h.parse::<i8>(), m.parse::<i8>())
        && b.len() <= 5
    {
        let today = now.date().at(h, m, 0, 0).to_zoned_datetime(TimeZone::system())?;
        let t = if today.timestamp() > now.timestamp() {
            today
        } else {
            today.tomorrow()?
        };
        return Ok(t.timestamp());
    }
    Ok(b.parse::<Timestamp>()?)
}

fn load_session() -> anyhow::Result<Session> {
    let path = Session::default_path();
    Session::load(&path)?.with_context(|| {
        format!(
            "no saved session at {}; run `evalfinder import-cookies <cookies.txt>` first",
            path.display()
        )
    })
}

async fn book(args: BookArgs) -> anyhow::Result<()> {
    let mut spec = JobSpec::new(args.project.clone(), args.count, deadline(&args)?);
    spec.buffer_minutes = args.buffer;
    spec.lead_minutes = args.lead;
    spec.interval_secs = args.interval;
    spec.dry_run = args.dry_run;
    let intra = IntraWeb::new(load_session()?)?;

    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let time = jiff::ZonedDateTime::now().strftime("%H:%M:%S");
            match ev {
                JobEvent::Log(l) => println!("{time} {l}"),
                JobEvent::Booked(_) => {}
            }
        }
    });
    let ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c.cancel();
        }
    });
    let result = megaft_core::job::run(intra.clone(), &NoRefresh, &spec, &cancel, &tx).await;
    drop(tx);
    printer.await?;
    intra.session().save(&Session::default_path())?;
    let (outcome, booked) = result?;
    println!(
        "done: {}/{} {} ({outcome:?})",
        booked.len(),
        spec.count,
        if spec.dry_run { "found" } else { "booked" }
    );
    Ok(())
}

async fn login(visible: bool, fresh: bool, user: Option<String>) -> anyhow::Result<()> {
    use megaft_browser::{Credentials, LoginOptions};
    let path = Session::default_path();
    let mut session = Session::load(&path)?.unwrap_or_else(Session::empty);
    let creds = match user {
        Some(login) => {
            let password = read_password(&format!("intra password for {login}: "))?;
            Some(Credentials { login, password })
        }
        None => None,
    };
    let mut opts = if visible {
        LoginOptions::visible()
    } else {
        LoginOptions::headless()
    };
    opts.fresh = fresh;
    let started = std::time::Instant::now();
    let logged = megaft_browser::login(&session.export()?, creds.as_ref(), &opts).await?;
    session.replace(logged.cookies, logged.user_agent)?;
    session.save(&path)?;
    println!(
        "logged in as {} in {:.1}s; session saved to {}",
        logged.login,
        started.elapsed().as_secs_f64(),
        path.display()
    );
    Ok(())
}

/// Reads a password from the terminal without echoing it.
fn read_password(prompt: &str) -> anyhow::Result<secrecy::SecretString> {
    use std::io::{BufRead as _, Write as _};
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let echo_off = std::process::Command::new("stty")
        .arg("-echo")
        .status()
        .is_ok_and(|s| s.success());
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if echo_off && std::process::Command::new("stty").arg("echo").status().is_err() {
        eprintln!("(could not restore terminal echo; run `stty echo`)");
    }
    eprintln!();
    read?;
    Ok(secrecy::SecretString::from(
        line.trim_end_matches(['\n', '\r']).to_owned(),
    ))
}

async fn status() -> anyhow::Result<()> {
    let intra = IntraWeb::new(load_session()?)?;
    match intra.whoami().await? {
        Some(who) => println!("logged in as {who}"),
        None => bail!("the saved session has expired"),
    }
    for p in intra.open_projects().await? {
        println!("  {:<28} {:<22} {}", p.slug, p.name, p.status);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        // Log lines would corrupt the TUI: only log (to a file) when EVALFINDER_LOG is set.
        if let Some(path) = std::env::var_os("EVALFINDER_LOG") {
            let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .init();
        }
        return tui::run(evalfinder_app::connect(cli.local).await).await;
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    match command {
        Command::Book(args) => book(args).await,
        Command::ImportCookies { path } => {
            let session = Session::import_netscape(&path, None)?;
            session.save(&Session::default_path())?;
            println!("saved session to {}", Session::default_path().display());
            Ok(())
        }
        Command::Status => status().await,
        Command::Login { visible, fresh, user } => login(visible, fresh, user).await,
    }
}
