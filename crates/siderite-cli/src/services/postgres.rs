//! The PostgreSQL recipe: one container per project, its data in a named
//! volume, published on 127.0.0.1 only, with generated credentials.

use super::runtime::{Presence, Runtime};
use super::state::{self, Postgres};
use super::{Failure, ServiceReport, Status};
use crate::error::CliError;
use crate::report::redact_text;
use std::path::Path;
use std::time::{Duration, Instant};

/// The image used unless `--image` or the recorded state names another.
pub(crate) const DEFAULT_IMAGE: &str = "postgres:17-alpine";

/// The service's name on the command line.
pub(crate) const NAME: &str = "postgres";

const USER: &str = "siderite";

type Step = Result<(), Failure>;
const POLL: Duration = Duration::from_millis(500);

/// The container and volume names of `state` for the package in `dir`.
pub(crate) struct Names {
    /// The container.
    pub container: String,
    /// The data volume.
    pub volume: String,
}

impl Names {
    /// Names scoped to the package directory's name and the random id.
    pub(crate) fn new(dir: &Path, state: &Postgres) -> Self {
        let project = slug(dir, '-');
        let base = format!("siderite-{project}-{}-postgres", state.id);
        Self {
            volume: format!("{base}-data"),
            container: base,
        }
    }
}

/// The directory's name in lowercase letters, digits and `separator`.
fn slug(dir: &Path, separator: char) -> String {
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let kept = name.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            separator
        }
    });
    let slug: String = kept.take(24).collect();
    let slug = slug.trim_matches(separator);
    if slug.is_empty() {
        "app".to_owned()
    } else {
        slug.to_owned()
    }
}

type Recorded = Result<Postgres, CliError>;

/// What `up` may change in the recorded state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Wanted {
    /// `--port`.
    pub port: Option<u16>,
    /// `--image`.
    pub image: Option<String>,
}

/// The recorded state, created with fresh credentials and a free local
/// port when there is none, and updated with `wanted`.
///
/// Errors:
///   An IO error reading or writing the state, or no free port.
pub(crate) fn prepare(dir: &Path, wanted: &Wanted) -> Recorded {
    let mut state = match state::load(dir)? {
        Some(state) => state,
        None => Postgres {
            id: random_hex(8),
            user: USER.to_owned(),
            password: random_hex(32),
            database: slug(dir, '_'),
            port: 0,
            image: DEFAULT_IMAGE.to_owned(),
        },
    };
    if let Some(image) = &wanted.image {
        state.image.clone_from(image);
    }
    if let Some(port) = wanted.port {
        state.port = port;
    }
    if state.port == 0 {
        state.port = free_port()?;
    }
    state::save(dir, &state)?;
    Ok(state)
}

/// `count` hexadecimal characters from the operating system's CSPRNG.
fn random_hex(count: usize) -> String {
    let mut out = String::new();
    while out.len() < count {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(count);
    out
}

fn free_port() -> Result<u16, CliError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0");
    let port = listener.and_then(|l| l.local_addr()).map(|a| a.port());
    port.map_err(|err| CliError::Io(format!("no free local port: {err}")))
}

/// The connection URL, with the password.
pub(crate) fn url(state: &Postgres) -> String {
    let Postgres {
        user,
        password,
        port,
        database,
        ..
    } = state;
    format!("postgres://{user}:{password}@127.0.0.1:{port}/{database}")
}

/// The PostgreSQL service of one package, operated through a runtime.
pub(crate) struct Service<'a> {
    runtime: &'a Runtime,
    dir: &'a Path,
    state: &'a Postgres,
    names: Names,
}

impl<'a> Service<'a> {
    /// The service recorded as `state` for the package in `dir`.
    pub fn new(rt: &'a Runtime, dir: &'a Path, state: &'a Postgres) -> Self {
        let names = Names::new(dir, state);
        Self {
            runtime: rt,
            dir,
            state,
            names,
        }
    }

    fn presence(&self) -> Result<Presence, Failure> {
        self.runtime.container(&self.names.container)
    }

    fn command(&self, args: &[&str]) -> Step {
        self.runtime.run(args, &[]).map(drop)
    }

    /// The service's report.
    ///
    /// Errors:
    ///   `services.E002` when the runtime cannot answer.
    pub(crate) fn report(&self) -> Result<ServiceReport, Failure> {
        let status = match self.presence()? {
            Presence::Running => Status::Running,
            Presence::Stopped => Status::Stopped,
            Presence::Absent => Status::Absent,
        };
        Ok(ServiceReport {
            name: NAME,
            status,
            data_volume: self.runtime.volume(&self.names.volume)?,
            container: self.names.container.clone(),
            volume: self.names.volume.clone(),
            image: self.state.image.clone(),
            address: format!("127.0.0.1:{}", self.state.port),
            url: redact_text(&url(self.state)),
        })
    }

    /// Start the service (creating its container when needed) and wait
    /// until it accepts connections.
    ///
    /// Errors:
    ///   `services.E002` when the runtime fails, `services.E003` when the
    ///   database is not ready within `timeout`.
    pub(crate) fn up(&self, timeout: Duration) -> Step {
        match self.presence()? {
            Presence::Running => {}
            Presence::Stopped => self.command(&["start", self.container()])?,
            Presence::Absent => self.create()?,
        }
        self.wait(timeout)
    }

    fn container(&self) -> &str {
        &self.names.container
    }

    fn create(&self) -> Step {
        let state = self.state;
        let publish = format!("127.0.0.1:{}:5432", state.port);
        let mount = format!("{}:/var/lib/postgresql/data", self.names.volume);
        let project = format!("dev.siderite.project={}", self.dir.display());
        let args = [
            "run",
            "--detach",
            "--name",
            self.container(),
            "--label",
            "dev.siderite.service=postgres",
            "--label",
            &project,
            "--publish",
            &publish,
            "--volume",
            &mount,
            "--env",
            "POSTGRES_USER",
            "--env",
            "POSTGRES_PASSWORD",
            "--env",
            "POSTGRES_DB",
            &state.image,
        ];
        let env = [
            ("POSTGRES_USER", state.user.as_str()),
            ("POSTGRES_PASSWORD", state.password.as_str()),
            ("POSTGRES_DB", state.database.as_str()),
        ];
        self.runtime.run(&args, &env).map(drop)
    }

    /// Poll `pg_isready` over TCP inside the container: the image's
    /// initialization server listens on a socket only, so this succeeds
    /// once the real server accepts connections.
    fn wait(&self, timeout: Duration) -> Step {
        let started = Instant::now();
        let args = [
            "exec",
            self.container(),
            "pg_isready",
            "--quiet",
            "--host=127.0.0.1",
            "--username",
            &self.state.user,
            "--dbname",
            &self.state.database,
        ];
        loop {
            if self.command(&args).is_ok() {
                return Ok(());
            }
            if self.presence()? != Presence::Running {
                let message = "stopped while starting";
                return Err(self.failure("services.E002", message, ""));
            }
            if started.elapsed() >= timeout {
                let secs = timeout.as_secs();
                let message = format!("was not ready after {secs}s");
                let more = ", or pass a longer --timeout";
                return Err(self.failure("services.E003", &message, more));
            }
            std::thread::sleep(POLL);
        }
    }

    fn failure(&self, code: &'static str, what: &str, more: &str) -> Failure {
        let (name, container) = (self.runtime.name(), self.container());
        Failure {
            code,
            message: format!("postgres ({container}) {what}"),
            hint: format!("see `{name} logs {container}`{more}"),
        }
    }

    /// Stop and remove the container; the data volume and the recorded
    /// state stay, so `up` resumes with the same data.
    ///
    /// Errors:
    ///   `services.E002` when the runtime fails.
    pub(crate) fn down(&self) -> Step {
        if self.presence()? == Presence::Absent {
            return Ok(());
        }
        self.command(&["stop", self.container()])?;
        self.command(&["rm", self.container()])
    }

    /// Remove the container and the data volume. The caller forgets the
    /// recorded state afterwards.
    ///
    /// Errors:
    ///   `services.E002` when the runtime fails.
    pub(crate) fn destroy(&self) -> Step {
        if self.presence()? != Presence::Absent {
            self.command(&["rm", "--force", self.container()])?;
        }
        let volume = &self.names.volume;
        if self.runtime.volume(volume)? {
            self.command(&["volume", "rm", volume])?;
        }
        Ok(())
    }
}
