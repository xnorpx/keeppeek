use clap::{Args, Subcommand, ValueEnum};
use keeppeek::{
    api::backup_proto,
    backup::{BackupClientError, BackupHttpClient},
};
use serde::Serialize;
use std::{collections::HashMap, fmt, path::PathBuf, str::FromStr};

#[derive(Args)]
#[command(name = "keeppeek backup")]
pub struct BackupArgs {
    #[arg(long, default_value = "http://localhost:3000")]
    server: String,
    #[command(subcommand)]
    command: BackupCommand,
}

#[derive(Subcommand)]
enum BackupCommand {
    /// Print server backup capabilities.
    Capabilities,
    /// List retained server backups.
    List,
    /// Create a managed backup, optionally downloading it.
    Create {
        #[arg(long = "section", value_enum)]
        sections: Vec<SectionArg>,
        #[arg(long, default_value_t = 0)]
        maximum_bytes: u64,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Upload and validate a ZIP bundle.
    Upload { path: PathBuf },
    /// Inspect one retained backup.
    Inspect { backup_id: String },
    /// Create an immutable restore dry run.
    DryRun {
        backup_id: String,
        #[arg(long = "section", value_enum)]
        sections: Vec<SectionArg>,
        #[arg(long = "map", value_name = "KIND=TARGET")]
        mappings: Vec<PathMappingArg>,
    },
    /// Stage an accepted restore plan for restart.
    Restore {
        plan_id: String,
        archive_sha256: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Print current restore or rollback status.
    Status { restore_id: String },
    /// Stage a rollback for restart.
    Rollback {
        restore_id: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Delete one retained backup.
    Delete { backup_id: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum SectionArg {
    RuntimeConfig,
    CameraDatabase,
    RecordingCatalog,
    EventMetadata,
    EventThumbnails,
    Groups,
    Layouts,
    Notifications,
    Integrations,
    Access,
    StateStore,
    ConfigurationTemplates,
}

impl SectionArg {
    const fn proto(self) -> backup_proto::BackupSection {
        match self {
            Self::RuntimeConfig => backup_proto::BackupSection::RuntimeConfig,
            Self::CameraDatabase => backup_proto::BackupSection::CameraDatabase,
            Self::RecordingCatalog => backup_proto::BackupSection::RecordingCatalog,
            Self::EventMetadata => backup_proto::BackupSection::EventMetadata,
            Self::EventThumbnails => backup_proto::BackupSection::EventThumbnails,
            Self::Groups => backup_proto::BackupSection::Groups,
            Self::Layouts => backup_proto::BackupSection::Layouts,
            Self::Notifications => backup_proto::BackupSection::Notifications,
            Self::Integrations => backup_proto::BackupSection::Integrations,
            Self::Access => backup_proto::BackupSection::Access,
            Self::StateStore => backup_proto::BackupSection::StateStore,
            Self::ConfigurationTemplates => backup_proto::BackupSection::ConfigurationTemplates,
        }
    }
}

#[derive(Clone)]
struct PathMappingArg {
    kind: backup_proto::BackupPathKind,
    target: String,
}

impl FromStr for PathMappingArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (kind, target) = value
            .split_once('=')
            .ok_or_else(|| "path mappings use KIND=TARGET".to_owned())?;
        if target.is_empty() {
            return Err("path mapping target must not be empty".to_owned());
        }
        let kind = match kind {
            "config-directory" => backup_proto::BackupPathKind::ConfigDirectory,
            "recording-catalog" => backup_proto::BackupPathKind::RecordingCatalog,
            "long-term-media" => backup_proto::BackupPathKind::LongTermMedia,
            "event-thumbnails" => backup_proto::BackupPathKind::EventThumbnails,
            "notification-database" => backup_proto::BackupPathKind::NotificationDatabase,
            _ => return Err(format!("unknown backup path kind {kind:?}")),
        };
        Ok(Self {
            kind,
            target: target.to_owned(),
        })
    }
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
        BackupCommand::Capabilities => print_json(&client.capabilities()?),
        BackupCommand::List => print_json(&client.list()?),
        BackupCommand::Create {
            sections,
            maximum_bytes,
            output,
        } => {
            let record = client.create(&backup_proto::CreateBackupRequest {
                client_request_id: uuid::Uuid::new_v4().to_string(),
                sections: sections
                    .into_iter()
                    .map(|section| section.proto() as i32)
                    .collect(),
                expected_archive_bytes: maximum_bytes,
            })?;
            if let Some(output) = output {
                client.download(&record.backup_id, &output, record.archive_bytes)?;
            }
            print_json(&record)
        }
        BackupCommand::Upload { path } => print_json(&client.upload(&path)?),
        BackupCommand::Inspect { backup_id } => print_json(&client.inspect(&backup_id)?),
        BackupCommand::DryRun {
            backup_id,
            sections,
            mappings,
        } => print_json(&dry_run(&client, &backup_id, sections, mappings)?),
        BackupCommand::Restore {
            plan_id,
            archive_sha256,
            confirm,
        } => {
            if !confirm {
                return Err(BackupCliError::Usage("restore requires --confirm"));
            }
            print_json(&client.activate(&backup_proto::ActivateRestoreRequest {
                client_request_id: uuid::Uuid::new_v4().to_string(),
                plan_id,
                archive_sha256,
                confirm,
            })?)
        }
        BackupCommand::Status { restore_id } => print_json(&client.get_restore(&restore_id)?),
        BackupCommand::Rollback {
            restore_id,
            confirm,
        } => {
            if !confirm {
                return Err(BackupCliError::Usage("rollback requires --confirm"));
            }
            print_json(&client.rollback(&backup_proto::RollbackRestoreRequest {
                client_request_id: uuid::Uuid::new_v4().to_string(),
                restore_id,
                confirm,
            })?)
        }
        BackupCommand::Delete { backup_id } => {
            print_json(&client.delete(&backup_proto::DeleteBackupRequest {
                client_request_id: uuid::Uuid::new_v4().to_string(),
                backup_id,
            })?)
        }
    }
}

fn dry_run(
    client: &BackupHttpClient,
    backup_id: &str,
    sections: Vec<SectionArg>,
    mappings: Vec<PathMappingArg>,
) -> Result<backup_proto::RestorePlan, BackupCliError> {
    let capabilities = client.capabilities()?;
    let backup = client.inspect(backup_id)?;
    let manifest = backup
        .manifest
        .ok_or(BackupCliError::Usage("backup manifest is unavailable"))?;
    let mut overrides = HashMap::with_capacity(mappings.len());
    for mapping in mappings {
        if overrides
            .insert(mapping.kind as i32, mapping.target)
            .is_some()
        {
            return Err(BackupCliError::Usage(
                "path mapping kinds must not be repeated",
            ));
        }
    }
    let path_mappings = manifest
        .source_paths
        .iter()
        .map(|source| {
            let target_path = overrides
                .get(&source.kind)
                .cloned()
                .or_else(|| {
                    capabilities
                        .target_paths
                        .iter()
                        .find(|target| target.kind == source.kind)
                        .map(|target| target.path.clone())
                })
                .ok_or(BackupCliError::Usage(
                    "a source path has no target; add --map KIND=TARGET",
                ))?;
            Ok(backup_proto::RestorePathMapping {
                kind: source.kind,
                source_path: source.path.clone(),
                target_path,
            })
        })
        .collect::<Result<Vec<_>, BackupCliError>>()?;
    client
        .create_restore_plan(&backup_proto::CreateRestorePlanRequest {
            client_request_id: uuid::Uuid::new_v4().to_string(),
            backup_id: backup_id.to_owned(),
            sections: sections
                .into_iter()
                .map(|section| section.proto() as i32)
                .collect(),
            path_mappings,
            expected_target_revision: capabilities.target_revision,
        })
        .map_err(Into::into)
}

fn print_json(value: &impl Serialize) -> Result<(), BackupCliError> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value).map_err(|_| BackupCliError::Output)?;
    use std::io::Write as _;
    output.write_all(b"\n").map_err(|_| BackupCliError::Output)
}
