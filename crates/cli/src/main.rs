use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
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
    /// Print the llmobs version.
    Version,
}

fn main() {
    let code = match tokio::runtime::Builder::new_multi_thread()
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
    }
    Ok(())
}

async fn serve(config_path: Option<&std::path::Path>) -> anyhow::Result<()> {
    let config = ServerConfig::load(config_path)
        .map_err(ServeError::from)
        .context("loading config")?;
    init_tracing(&config.log)?;
    let signals = Signals::install().context("installing signal handlers")?;
    serve::run(config, signals.wait()).await.map_err(annotate)?;
    Ok(())
}

fn annotate(error: ServeError) -> anyhow::Error {
    let context = match &error {
        ServeError::Config(_) => "loading config",
        ServeError::Storage(_) => "opening storage",
        ServeError::Meta(_) => "opening meta.db",
        ServeError::Bind { .. } => "binding",
        ServeError::Serve(_) => "serving",
    };
    anyhow::Error::from(error).context(context)
}

fn init_tracing(log: &LogConfig) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(&log.level).context("loading config")?;
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
    writeln!(stdout, "llmobs {}", env!("CARGO_PKG_VERSION"))
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
    use super::run_cli;

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
}
