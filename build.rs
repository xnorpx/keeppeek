use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

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
    let status = Command::new("bun")
        .args(["run", "build"])
        .current_dir(&ui_dir)
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

    if !ui_dir.join("build/index.html").is_file() {
        return Err(io::Error::other(
            "KeepPeek UI build did not produce ui/build/index.html",
        ));
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
