fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities/default.json");
    println!("cargo:rerun-if-changed=frontend/index.html");
    println!("cargo:rerun-if-changed=frontend/styles.css");
    println!("cargo:rerun-if-changed=frontend/app.js");
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    tauri_build::build();
}
