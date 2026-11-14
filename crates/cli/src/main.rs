mod login;

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, SecondsFormat};
use clap::{Parser, Subcommand, ValueEnum};
use llmobs_server::admin::{self, UserSummary};
use llmobs_server::config::{LogConfig, LogFormat, ServerConfig};
use llmobs_server::serve::{self, ServeError};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(name = "llmobs", version, about = "LLM observability server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the HTTP server.
    Serve {
        /// Config file. Overrides `LLMOBS_CONFIG`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Manage users in this data directory.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Print the llmobs version.
    Version,
    /// Save a personal access token for a server.
    Login {
        /// Server URL, for example `http://127.0.0.1:8080`.
        #[arg(long)]
        url: String,
        /// Print the login URL and do not open a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Revoke the saved personal access token.
    Logout {
        /// Server URL. Required when more than one server is saved.
        #[arg(long)]
        url: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Create a user.
    CreateUser {
        /// Email address.
        #[arg(long)]
        email: String,
        /// Display name.
        #[arg(long)]
        name: Option<String>,
        /// Read the password from one line of stdin.
        #[arg(long)]
        password_stdin: bool,
        /// Make the user an instance admin.
        #[arg(long)]
        instance_admin: bool,
        /// Config file. Overrides `LLMOBS_CONFIG`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Print a password reset URL.
    ResetPassword {
        /// Email address.
        #[arg(long)]
        email: String,
        /// Config file. Overrides `LLMOBS_CONFIG`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// List users.
    ListUsers {
        /// `table` or `json`.
        #[arg(long, value_enum, default_value = "table")]
        format: ListFormat,
        /// Config file. Overrides `LLMOBS_CONFIG`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ListFormat {
    Table,
    Json,
}

fn main() {
    let code = match tokio::runtime::Builder::new_multi_thread()
        // A2 asks for at least 8 MiB per worker: DataFusion plans recursively (A2 plan § Boundaries).
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(async_main()),
        Err(error) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "starting the runtime: {error}");
            1
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}

async fn async_main() -> i32 {
    match run_cli(std::env::args_os()).await {
        Ok(()) => 0,
        Err(error) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{error:#}");
            1
        }
    }
}

/// Run one CLI invocation. `main` turns `Err` into exit status 1.
async fn run_cli(
    args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
) -> anyhow::Result<()> {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            error.print()?;
            if error.exit_code() == 0 {
                return Ok(());
            }
            std::process::exit(error.exit_code());
        }
    };
    match cli.command {
        Command::Version => print_version()?,
        Command::Serve { config } => serve(config.as_deref()).await?,
        Command::Admin { command } => admin_command(command).await?,
        Command::Login { url, no_browser } => {
            login::login(&url, no_browser, &credentials_path()?, io::stdout()).await?;
        }
        Command::Logout { url } => login::logout(url.as_deref(), &credentials_path()?).await?,
    }
    Ok(())
}

async fn serve(config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = ServerConfig::load(config_path)
        .map_err(ServeError::from)
        .context("loading config")?;
    init_tracing(&config.log)?;
    let signals = Signals::install().context("installing signal handlers")?;
    serve::run(config, Some(llmobs_ui::router()), signals.wait())
        .await
        .map_err(annotate)?;
    Ok(())
}

fn annotate(error: ServeError) -> anyhow::Error {
    let context = match &error {
        ServeError::Config(_) => "loading config",
        ServeError::Storage(_) => "opening storage",
        ServeError::Meta(_) => "opening meta.db",
        ServeError::Bind { .. } => "binding",
        ServeError::Prices(_) => "building the price table",
        ServeError::Serve(_) => "serving",
        ServeError::Init { .. } => "applying init",
    };
    anyhow::Error::from(error).context(context)
}

/// The `rmcp` crate logs whole MCP requests and results below `warn`; they can hold prompts
/// and SQL text, so its level is capped whatever `log.level` says (README § Privacy).
const RMCP_DIRECTIVE: &str = "rmcp=warn";

/// `log.level` as an `EnvFilter`, with [`RMCP_DIRECTIVE`] added last so it replaces any
/// `rmcp=…` directive in `level`.
fn log_filter(level: &str) -> anyhow::Result<EnvFilter> {
    let directive = RMCP_DIRECTIVE
        .parse()
        .context("parsing the rmcp log directive")?;
    Ok(EnvFilter::try_new(level)
        .context("loading config")?
        .add_directive(directive))
}

fn init_tracing(log: &LogConfig) -> anyhow::Result<()> {
    let filter = log_filter(&log.level)?;
    let format = log.format.unwrap_or_else(default_log_format);
    match format {
        LogFormat::Pretty => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .pretty()
                .with_writer(io::stdout)
                .init();
        }
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .with_writer(io::stdout)
                .init();
        }
    }
    Ok(())
}

fn default_log_format() -> LogFormat {
    if io::stdout().is_terminal() {
        LogFormat::Pretty
    } else {
        LogFormat::Json
    }
}

fn print_version() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "llmobs {}", env!("CARGO_PKG_VERSION"))?;
    let ui = if llmobs_ui::embedded() {
        "embedded"
    } else {
        "not built"
    };
    writeln!(stdout, "ui: {ui}")
}

async fn admin_command(command: AdminCommand) -> anyhow::Result<()> {
    match command {
        AdminCommand::CreateUser {
            email,
            name,
            password_stdin,
            instance_admin,
            config,
        } => {
            let password = if password_stdin {
                Some(read_password_line(io::stdin().lock())?)
            } else {
                None
            };
            let loaded = load_config(config.as_deref())?;
            let created = admin::create_user(
                &loaded,
                &email,
                name.as_deref(),
                password.as_deref(),
                instance_admin,
            )
            .await
            .context("creating the user")?;
            let mut stdout = io::stdout().lock();
            if let Some(url) = created.reset_url {
                writeln!(stdout, "Set the password at: {url}")?;
            } else {
                writeln!(stdout, "Created {}", created.id)?;
            }
        }
        AdminCommand::ResetPassword { email, config } => {
            let loaded = load_config(config.as_deref())?;
            let url = admin::reset_password(&loaded, &email)
                .await
                .context("resetting the password")?;
            writeln!(io::stdout().lock(), "{url}")?;
        }
        AdminCommand::ListUsers { format, config } => {
            let loaded = load_config(config.as_deref())?;
            let users = admin::list_users(&loaded).await.context("listing users")?;
            let mut stdout = io::stdout().lock();
            match format {
                ListFormat::Table => write_table(&mut stdout, &users)?,
                ListFormat::Json => {
                    let listed = users.iter().map(listed_user).collect::<Vec<_>>();
                    writeln!(stdout, "{}", serde_json::to_string_pretty(&listed)?)?;
                }
            }
        }
    }
    Ok(())
}

fn load_config(path: Option<&Path>) -> anyhow::Result<ServerConfig> {
    ServerConfig::load(path).context("loading config")
}

fn credentials_path() -> anyhow::Result<PathBuf> {
    let config = dirs::config_dir().context("finding the config directory")?;
    Ok(config.join("llmobs").join("credentials.toml"))
}

/// One stdin line, with a trailing `\n` or `\r\n` removed. The password itself is not trimmed.
fn read_password_line(mut input: impl BufRead) -> anyhow::Result<String> {
    let mut line = String::new();
    let read = input.read_line(&mut line).context("reading the password")?;
    if read == 0 {
        anyhow::bail!("expected a password on stdin");
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

fn write_table(out: &mut impl Write, users: &[UserSummary]) -> io::Result<()> {
    writeln!(out, "id\temail\tname\tadmin\tdisabled\tcreated")?;
    for user in users {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}",
            user.id,
            user.email,
            user.name.as_deref().unwrap_or(""),
            yes_no(user.instance_admin),
            yes_no(user.disabled),
            format_time(user.created_at),
        )?;
    }
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn format_time(ns: i64) -> String {
    if ns < 0 {
        return String::new();
    }
    let seconds = ns.div_euclid(1_000_000_000);
    let nanos = ns.rem_euclid(1_000_000_000);
    let Ok(subsec) = u32::try_from(nanos) else {
        return String::new();
    };
    DateTime::from_timestamp(seconds, subsec)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Nanos, true))
        .unwrap_or_default()
}

#[derive(serde::Serialize)]
struct ListedUser {
    id: String,
    email: String,
    name: Option<String>,
    instance_admin: bool,
    disabled: bool,
    created_at: String,
}

fn listed_user(user: &UserSummary) -> ListedUser {
    ListedUser {
        id: user.id.to_string(),
        email: user.email.clone(),
        name: user.name.clone(),
        instance_admin: user.instance_admin,
        disabled: user.disabled,
        created_at: format_time(user.created_at),
    }
}

/// SIGINT and SIGTERM. A second signal exits with status 130.
struct Signals {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
}

impl Signals {
    fn install() -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                term: signal(SignalKind::terminate()).context("installing SIGTERM handler")?,
                int: signal(SignalKind::interrupt()).context("installing SIGINT handler")?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn wait(mut self) {
        self.recv().await;
        tokio::spawn(async move {
            self.recv().await;
            std::process::exit(130);
        });
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.term.recv() => {}
                _ = self.int.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{log_filter, run_cli};

    #[test]
    fn log_filter_caps_rmcp() {
        for level in ["info", "trace", "info,rmcp=trace"] {
            let filter = log_filter(level).unwrap().to_string();
            assert!(filter.contains("rmcp=warn"), "{level}: {filter}");
            assert!(!filter.contains("rmcp=trace"), "{level}: {filter}");
        }
    }

    #[tokio::test]
    async fn missing_config_exits_with_the_path() {
        let path = std::env::temp_dir().join(format!("llmobs-missing-{}.toml", std::process::id()));
        let error = run_cli(["llmobs", "serve", "--config", path.to_str().unwrap()])
            .await
            .unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("loading config"), "{text}");
        assert!(text.contains(&path.display().to_string()), "{text}");
    }

    #[test]
    fn password_line_drops_one_trailing_newline() {
        let line = super::read_password_line("secret\r\n".as_bytes()).unwrap();
        assert_eq!(line, "secret");
        let spaced = super::read_password_line(" secret \n".as_bytes()).unwrap();
        assert_eq!(spaced, " secret ");
        let eof = super::read_password_line("secret".as_bytes()).unwrap();
        assert_eq!(eof, "secret");
        assert!(super::read_password_line("".as_bytes()).is_err());
    }

    #[tokio::test]
    async fn list_users_on_a_missing_database_names_serve() {
        let dir = std::env::temp_dir().join(format!(
            "llmobs-admin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("llmobs.toml");
        let data = dir.join("data");
        std::fs::write(&config_path, format!("data_dir = \"{}\"\n", data.display())).unwrap();
        let error = run_cli([
            "llmobs",
            "admin",
            "list-users",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .await
        .unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains("run `llmobs serve` once to create the database"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
