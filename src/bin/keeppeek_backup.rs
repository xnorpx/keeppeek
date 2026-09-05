use clap::{Args, Subcommand};
use keeppeek::backup::{BackupClientError, BackupHttpClient};
use serde::Serialize;
use std::{fmt, path::PathBuf};

#[derive(Args)]
#[command(name = "keeppeek config")]
pub struct BackupArgs {
    #[arg(long, default_value = "http://localhost:3000")]
    server: String,
    #[command(subcommand)]
    command: BackupCommand,
}

#[derive(Subcommand)]
enum BackupCommand {
    /// Download config.toml and plaintext secrets.toml as a ZIP archive.
    Export {
        #[arg(long)]
        output: PathBuf,
    },
    /// Validate a ZIP archive and stage both configuration files for restart.
    Apply {
        path: PathBuf,
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Debug)]
pub enum BackupCliError {
    Client(BackupClientError),
    Usage(&'static str),
    Output,
}

impl BackupCliError {
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Client(error) => error.exit_code(),
            Self::Usage(_) => 2,
            Self::Output => 1,
        }
    }
}

impl fmt::Display for BackupCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(error) => error.fmt(formatter),
            Self::Usage(message) => formatter.write_str(message),
            Self::Output => formatter.write_str("backup command output failed"),
        }
    }
}

impl std::error::Error for BackupCliError {}

impl From<BackupClientError> for BackupCliError {
    fn from(error: BackupClientError) -> Self {
        Self::Client(error)
    }
}

pub fn run(args: BackupArgs) -> Result<(), BackupCliError> {
    let access_key = match std::env::var("KEEPPEEK_ACCESS_KEY") {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(BackupCliError::Usage(
                "KEEPPEEK_ACCESS_KEY is not valid Unicode",
            ));
        }
    };
    let client = BackupHttpClient::new(&args.server, access_key)?;
    match args.command {
        BackupCommand::Export { output } => print_json(&serde_json::json!({
            "archiveBytes": client.export(&output)?.to_string(),
        })),
        BackupCommand::Apply { path, confirm } => {
            if !confirm {
                return Err(BackupCliError::Usage(
                    "configuration apply requires --confirm",
                ));
            }
            print_json(&client.apply(&path)?)
        }
    }
}

fn print_json(value: &impl Serialize) -> Result<(), BackupCliError> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value).map_err(|_| BackupCliError::Output)?;
    use std::io::Write as _;
    output.write_all(b"\n").map_err(|_| BackupCliError::Output)
}
