//! The three screens as plain state. UIs draw these and feed user input/backend results back.

use jiff::{SignedDuration, Timestamp, ZonedDateTime, tz::TimeZone};
use megaft_core::{Booked, JobSpec, Project};
use secrecy::SecretString;

use crate::JobUpdate;

/// Login screen.
#[derive(Debug, Default, Clone)]
pub struct LoginForm {
    pub login: String,
    pub password: String,
    pub error: Option<String>,
    pub busy: Option<String>,
}

impl LoginForm {
    /// Login + password ready to send, or an error to show.
    pub fn credentials(&self) -> Result<(String, SecretString), String> {
        let login = self.login.trim();
        if login.is_empty() || self.password.is_empty() {
            return Err("Enter your login and password.".into());
        }
        Ok((login.to_owned(), SecretString::from(self.password.clone())))
    }

    /// Forget the typed password (after it's been handed to the backend).
    pub fn clear_password(&mut self) {
        // Overwrite before dropping so the bytes don't linger in this buffer.
        let len = self.password.len();
        self.password.clear();
        self.password.extend(std::iter::repeat_n('\0', len));
        self.password.clear();
    }
}

/// Setup screen.
#[derive(Debug, Clone)]
pub struct SetupForm {
    pub who: String,
    pub backend: &'static str,
    pub projects: Vec<Project>,
    pub selected: Option<usize>,
    pub count: String,
    pub hours: String,
    pub dry_run: bool,
    pub error: Option<String>,
    pub loading: bool,
}

impl SetupForm {
    #[must_use]
    pub fn new(who: String, backend: &'static str) -> Self {
        Self {
            who,
            backend,
            projects: Vec::new(),
            selected: None,
            count: "1".into(),
            hours: "4".into(),
            dry_run: false,
            error: None,
            loading: true,
        }
    }

    /// Fills the project list and preselects the first one that needs an evaluation.
    pub fn set_projects(&mut self, projects: Vec<Project>) {
        self.selected = projects
            .iter()
            .position(|p| p.status.to_lowercase().contains("evaluation"))
            .or_else(|| (!projects.is_empty()).then_some(0));
        self.projects = projects;
        self.loading = false;
    }

    #[must_use]
    pub fn selected_project(&self) -> Option<&Project> {
        self.selected.and_then(|i| self.projects.get(i))
    }

    /// The deadline the hours field means right now, if it parses.
    #[must_use]
    pub fn deadline(&self, now: Timestamp) -> Option<Timestamp> {
        let hours: f64 = self.hours.trim().parse().ok()?;
        if !hours.is_finite() || hours <= 0.0 {
            return None;
        }
        let d = SignedDuration::try_from_secs_f64(hours * 3600.0).ok()?;
        now.checked_add(d).ok()
    }

    /// "Evaluations must start before Tue 24 Nov 02:57".
    #[must_use]
    pub fn deadline_hint(&self, now: Timestamp) -> String {
        self.deadline(now).map_or_else(String::new, |d| {
            format!("Evaluations must start before {}", local_long(d))
        })
    }

    /// The job to start, or an error to show.
    pub fn job_spec(&self, now: Timestamp) -> Result<JobSpec, String> {
        let project = self.selected_project().ok_or("Choose a project.")?;
        let count: u32 = self
            .count
            .trim()
            .parse()
            .map_err(|e| format!("Number of evaluations must be a whole number ({e})."))?;
        if count == 0 {
            return Err("Book at least 1 evaluation.".into());
        }
        let deadline = self.deadline(now).ok_or("Hours must be a positive number.")?;
        let mut spec = JobSpec::new(project.slug.clone(), count, deadline);
        spec.dry_run = self.dry_run;
        Ok(spec)
    }
}

/// Run screen.
#[derive(Debug, Clone)]
pub struct RunView {
    pub spec: JobSpec,
    pub status: String,
    pub log: Vec<(Timestamp, String)>,
    pub booked: Vec<Booked>,
    /// `Some(summary)` once the job stopped.
    pub finished: Option<(String, String)>,
    pub stopping: bool,
}

impl RunView {
    #[must_use]
    pub fn new(spec: JobSpec) -> Self {
        Self {
            spec,
            status: "Starting…".into(),
            log: Vec::new(),
            booked: Vec::new(),
            finished: None,
            stopping: false,
        }
    }

    /// Applies an update; returns the booking if it was one (UIs notify/beep on it).
    pub fn apply(&mut self, update: JobUpdate) -> Option<Booked> {
        match update {
            JobUpdate::Log(t, line) => {
                self.status.clone_from(&line);
                self.log.push((t, line));
                None
            }
            JobUpdate::Booked(b) => {
                self.booked.push(b.clone());
                Some(b)
            }
            JobUpdate::Finished { state, summary } => {
                self.status.clone_from(&summary);
                self.log.push((Timestamp::now(), summary.clone()));
                self.finished = Some((state, summary));
                None
            }
        }
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.finished.is_none()
    }

    /// "codexion: 2 evaluation(s) starting before Tue 24 Nov 02:57 (dry run)".
    #[must_use]
    pub fn title(&self) -> String {
        format!(
            "{}: {} evaluation(s) starting before {}{}",
            self.spec.project,
            self.spec.count,
            local_long(self.spec.deadline),
            if self.spec.dry_run { " (dry run)" } else { "" }
        )
    }
}

fn zoned(t: Timestamp) -> ZonedDateTime {
    t.to_zoned_datetime(TimeZone::system())
}

/// "Tue 24 Nov 02:57"
#[must_use]
pub fn local_long(t: Timestamp) -> String {
    zoned(t).strftime("%a %d %b %H:%M").to_string()
}

/// "Tue 24 Nov"
#[must_use]
pub fn local_day(t: Timestamp) -> String {
    zoned(t).strftime("%a %d %b").to_string()
}

/// "09:45"
#[must_use]
pub fn local_hm(t: Timestamp) -> String {
    zoned(t).strftime("%H:%M").to_string()
}

/// "23:59:01"
#[must_use]
pub fn local_hms(t: Timestamp) -> String {
    zoned(t).strftime("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(slug: &str, status: &str) -> Project {
        Project {
            slug: slug.into(),
            name: slug.into(),
            status: status.into(),
        }
    }

    #[test]
    fn preselects_project_needing_evaluation() {
        let mut f = SetupForm::new("me".into(), "test");
        f.set_projects(vec![
            project("a", "In progress"),
            project("b", "Evaluation needed"),
        ]);
        assert_eq!(f.selected_project().unwrap().slug, "b");
    }

    #[test]
    fn validates_fields() {
        let now: Timestamp = "2026-11-24T00:00:00Z".parse().unwrap();
        let mut f = SetupForm::new("me".into(), "test");
        assert_eq!(f.job_spec(now).unwrap_err(), "Choose a project.");
        f.set_projects(vec![project("codexion", "Evaluation needed")]);
        f.count = "x".into();
        assert!(f.job_spec(now).unwrap_err().contains("whole number"));
        f.count = "2".into();
        f.hours = "-1".into();
        assert!(f.job_spec(now).unwrap_err().contains("positive"));
        f.hours = "1.5".into();
        let spec = f.job_spec(now).unwrap();
        assert_eq!(spec.count, 2);
        assert_eq!(spec.deadline.to_string(), "2026-11-24T01:30:00Z");
    }

    #[test]
    fn login_needs_both_fields() {
        let mut l = LoginForm {
            login: " me ".into(),
            ..LoginForm::default()
        };
        assert_eq!(l.credentials().unwrap_err(), "Enter your login and password.");
        l.password = "pw".into();
        assert_eq!(l.credentials().unwrap().0, "me");
        l.clear_password();
        assert!(l.password.is_empty());
    }

    #[test]
    fn run_view_tracks_updates() {
        let now: Timestamp = "2026-11-24T00:00:00Z".parse().unwrap();
        let mut r = RunView::new(JobSpec::new("codexion".into(), 1, now));
        assert!(r.apply(JobUpdate::Log(now, "polling".into())).is_none());
        let b = Booked {
            start: now,
            end: now,
            dry_run: true,
        };
        assert_eq!(r.apply(JobUpdate::Booked(b.clone())), Some(b));
        r.apply(JobUpdate::Finished {
            state: "done".into(),
            summary: "Done: 1/1 found.".into(),
        });
        assert!(!r.is_running());
        assert_eq!(r.status, "Done: 1/1 found.");
    }
}
