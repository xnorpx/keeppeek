use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    env, fs,
    hash::{DefaultHasher, Hash, Hasher},
    io,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

const CAMERA_DATABASE_ARCHIVE_URL: &str =
    "https://github.com/ch-bas/cctv-camera-database/releases/download/v2.8.0/cameras.zip";
const CAMERA_DATABASE_ARCHIVE_SHA256: &str =
    "9b86ff8d4afa8721ab115e3fd0b04ca33a4b28e5b519d2283ea2ef68a0c8f009";
const CAMERA_DATABASE_ARCHIVE_ENV: &str = "KEEPPEEK_CAMERA_DATABASE_ARCHIVE";
const CAMERA_DATABASE_ARCHIVE_FILE: &str = "cameras.zip";
const CAMERA_DATABASE_FILES: &[&str] = &["cameras.json", "cameras.csv", "release-metadata.json"];
const CAMERA_DATABASE_DOWNLOAD_ATTEMPTS: usize = 3;
const UI_BUILD_DIR_ENV: &str = "KEEPPEEK_UI_BUILD_DIR";

const UI_INPUTS: &[&str] = &[
    "ui/src",
    "ui/.bun-version",
    "ui/bunfig.toml",
    "ui/components.json",
    "ui/package.json",
    "ui/svelte.config.js",
    "ui/tsconfig.json",
    "ui/vite.config.ts",
];

const OPTIONAL_UI_INPUTS: &[&str] = &["ui/static"];

fn main() -> io::Result<()> {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must set CARGO_MANIFEST_DIR"),
    );

    println!("cargo:rerun-if-env-changed={CAMERA_DATABASE_ARCHIVE_ENV}");
    download_camera_database()?;

    println!("cargo:rerun-if-changed=api/backup.proto");
    println!("cargo:rerun-if-changed=api/webrtc.proto");
    let protoc = protoc_bin_vendored::protoc_bin_path()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let descriptor_path = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"))
        .join("keeppeek-proto-descriptor.bin");
    let mut prost_config = prost_build::Config::new();
    prost_config.protoc_executable(protoc);
    prost_config.file_descriptor_set_path(&descriptor_path);
    prost_config.compile_protos(&["api/backup.proto", "api/webrtc.proto"], &["api/"])?;
    let descriptors = fs::read(descriptor_path)?;
    pbjson_build::Builder::new()
        .register_descriptors(&descriptors)
        .map_err(|error| io::Error::other(error.to_string()))?
        .build(&[".keeppeek.backup.v1"])
        .map_err(|error| io::Error::other(error.to_string()))?;

    for input in UI_INPUTS {
        emit_rerun_if_changed(&manifest_dir.join(input), true)?;
    }
    for input in OPTIONAL_UI_INPUTS {
        emit_rerun_if_changed(&manifest_dir.join(input), false)?;
    }

    let ui_dir = manifest_dir.join("ui");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let mut out_dir_hasher = DefaultHasher::new();
    out_dir.hash(&mut out_dir_hasher);
    let cargo_ui_dir = ui_dir
        .join(".cargo-ui")
        .join(format!("{:016x}", out_dir_hasher.finish()));
    let ui_build_dir = cargo_ui_dir.join("build");
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(&ui_dir)
        .env(UI_BUILD_DIR_ENV, &ui_build_dir)
        .status()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to start Bun for the KeepPeek UI: {error}. Install Bun, then run `bun install` in {}",
                    ui_dir.display()
                ),
            )
        })?;
    if !status.success() {
        return Err(io::Error::other(
            "KeepPeek UI build failed. Run `bun install` in ui and try again.",
        ));
    }

    if !ui_build_dir.join("index.html").is_file() {
        return Err(io::Error::other(format!(
            "KeepPeek UI build did not produce {}",
            ui_build_dir.join("index.html").display()
        )));
    }
    println!(
        "cargo:rustc-env={UI_BUILD_DIR_ENV}={}",
        ui_build_dir.display()
    );

    Ok(())
}

fn download_camera_database() -> io::Result<()> {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let destination = out_dir.join(CAMERA_DATABASE_ARCHIVE_FILE);
    let bytes = match env::var_os(CAMERA_DATABASE_ARCHIVE_ENV) {
        Some(path) => {
            let path = PathBuf::from(path);
            println!("cargo:rerun-if-changed={}", path.display());
            fs::read(&path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "failed to read {CAMERA_DATABASE_ARCHIVE_ENV} archive at {}: {error}",
                        path.display()
                    ),
                )
            })?
        }
        None => download_camera_database_archive()?,
    };

    validate_camera_database_archive_digest(&bytes)?;
    validate_camera_database_archive(&bytes)?;
    fs::write(destination, bytes)
}

fn validate_camera_database_archive_digest(bytes: &[u8]) -> io::Result<()> {
    let actual = encode_lower_hex(Sha256::digest(bytes));
    if actual != CAMERA_DATABASE_ARCHIVE_SHA256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "camera database archive SHA-256 {actual} does not match {CAMERA_DATABASE_ARCHIVE_SHA256}"
            ),
        ));
    }
    Ok(())
}

fn encode_lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn download_camera_database_archive() -> io::Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut last_error = None;
    for attempt in 1..=CAMERA_DATABASE_DOWNLOAD_ATTEMPTS {
        let response = match agent.get(CAMERA_DATABASE_ARCHIVE_URL).call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(status)) => {
                return Err(io::Error::other(format!(
                    "camera database download returned HTTP {status}"
                )));
            }
            Err(error) => {
                last_error = Some(format!("failed to download camera database: {error}"));
                if attempt < CAMERA_DATABASE_DOWNLOAD_ATTEMPTS {
                    eprintln!(
                        "camera database download attempt {attempt} failed: {error}; retrying"
                    );
                    thread::sleep(Duration::from_secs(attempt as u64));
                    continue;
                }
                break;
            }
        };
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "camera database download returned HTTP {}",
                response.status()
            )));
        }
        let mut body = response.into_body();
        match body.read_to_vec() {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                last_error = Some(format!("failed to read camera database: {error}"));
                if attempt < CAMERA_DATABASE_DOWNLOAD_ATTEMPTS {
                    eprintln!(
                        "camera database download attempt {attempt} failed while reading: {error}; retrying"
                    );
                    thread::sleep(Duration::from_secs(attempt as u64));
                }
            }
        }
    }
    Err(io::Error::other(last_error.unwrap_or_else(|| {
        "failed to download camera database".to_owned()
    })))
}

fn validate_camera_database_archive(bytes: &[u8]) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut entries = HashSet::with_capacity(archive.len());

    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let name = entry.name().to_owned();
        if name.starts_with('/')
            || name.contains('\\')
            || name
                .split('/')
                .any(|component| matches!(component, "." | ".."))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("camera database archive contains unsafe entry {name:?}"),
            ));
        }
        if !entries.insert(name.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("camera database archive contains duplicate entry {name:?}"),
            ));
        }
    }

    for required in CAMERA_DATABASE_FILES {
        if !entries.contains(*required) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("camera database archive is missing {required}"),
            ));
        }
    }

    Ok(())
}

fn emit_rerun_if_changed(path: &Path, required: bool) -> io::Result<()> {
    println!("cargo:rerun-if-changed={}", path.display());
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if !required && error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                println!("cargo:rerun-if-changed={}", parent.display());
            }
            return Ok(());
        }
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "declared build input {} is unavailable: {error}",
                    path.display()
                ),
            ));
        }
    };
    if !metadata.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        emit_rerun_if_changed(&entry?.path(), required)?;
    }
    Ok(())
}
