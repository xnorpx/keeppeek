use std::{
    collections::HashSet,
    env, fs,
    hash::{DefaultHasher, Hash, Hasher},
    io,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
};

const CAMERA_DATABASE_ARCHIVE_URL: &str =
    "https://github.com/ch-bas/cctv-camera-database/releases/latest/download/cameras.zip";
const CAMERA_DATABASE_ARCHIVE_ENV: &str = "KEEPPEEK_CAMERA_DATABASE_ARCHIVE";
const CAMERA_DATABASE_ARCHIVE_FILE: &str = "cameras.zip";
const CAMERA_DATABASE_FILES: &[&str] = &["cameras.json", "cameras.csv", "release-metadata.json"];
const UI_BUILD_DIR_ENV: &str = "KEEPPEEK_UI_BUILD_DIR";
const SVELTE_KIT_DIR_ENV: &str = "KEEPPEEK_SVELTE_KIT_DIR";

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

    println!("cargo:rerun-if-changed=api/webrtc.proto");
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());
    }
    prost_build::compile_protos(&["api/webrtc.proto"], &["api/"])?;

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
    let svelte_kit_dir = cargo_ui_dir.join("svelte-kit");
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(&ui_dir)
        .env(UI_BUILD_DIR_ENV, &ui_build_dir)
        .env(SVELTE_KIT_DIR_ENV, &svelte_kit_dir)
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

    validate_camera_database_archive(&bytes)?;
    fs::write(destination, bytes)
}

fn download_camera_database_archive() -> io::Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build();
    let response = ureq::Agent::new_with_config(config)
        .get(CAMERA_DATABASE_ARCHIVE_URL)
        .call()
        .map_err(|error| {
            io::Error::other(format!("failed to download camera database: {error}"))
        })?;
    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "camera database download returned HTTP {}",
            response.status()
        )));
    }
    let mut body = response.into_body();
    body.read_to_vec()
        .map_err(|error| io::Error::other(format!("failed to read camera database: {error}")))
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
