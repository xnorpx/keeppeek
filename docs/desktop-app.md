# KeepPeek Desktop engineering and verification

`crates/keeppeek-desktop` implements the native Windows and macOS Tauri v2 desktop client for KeepPeek.

## Architecture and deployment boundary

By default, `keeppeek-desktop` is a connect-to-existing-server client:

- In default connect-only mode, it never spawns, supervises, or terminates `keeppeek`, `keeppeek-service`, or the macOS `com.keeppeek` LaunchAgent.
- When an operator explicitly opts in via `--manage-local-server`, `[desktop].manage_local_server = true`, or the local launcher toggle, trusted Rust code embeds `crates/keeppeek-supervisor` to supervise a local `keeppeek` child process (using versioned A/B slots under `[desktop].supervisor_root` or `<config_dir>/supervisor`, falling back to sibling `keeppeek[.exe]`), connects once ready, and stops only its own supervised child on quit.
- It reads and writes only `[desktop]` keys (`server_url`, `manage_local_server`, `supervisor_root`) inside the existing canonical `config.toml` (see `src/config.rs` and `crates/keeppeek-desktop/src/config_store.rs`), preserving all other sections and `{secret:...}` references.
- It validates and probes `<origin>/auth/session` with explicit timeouts (5 s default), zero redirects, and a 64 KiB body cap before navigating the main webview to `<origin>/`.
- On Linux, the crate builds and executes all platform-independent tests (`server_url`, `navigation`, `config_store`, `managed_server`, `probe`, and Tauri configuration security checks) without requiring WebKitGTK, while gating the `tauri` runtime behind `cfg(any(target_os = "windows", target_os = "macos"))`.

## Security invariants

1. `crates/keeppeek-desktop/tauri.conf.json` sets `"withGlobalTauri": false` and a strict CSP (`default-src 'self'`, `object-src 'none'`, `frame-ancestors 'none'`, no `'unsafe-inline'`, no `'unsafe-eval'`, no wildcards).
2. `crates/keeppeek-desktop/capabilities/default.json` grants `"local": true` only to the `"main"` window with no `"remote"` URLs and no `shell`, `fs`, `http`, or `process` plugin permissions.
3. `crates/keeppeek-desktop/src/navigation.rs` allows top-level navigation only to the bundled launcher origin (`tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`) or the active verified KeepPeek server origin, and denies all popup windows (`NewWindowResponse::Deny`). As a deliberate consequence of this boundary, cross-origin OIDC redirects (`POST /auth/login` to an external IdP) and popup-based administrator OIDC verification (`window.open`) are blocked in the desktop webview; operators should use a standard browser for those OIDC flows while trusted-local, bearer access-key, and same-origin proxy sessions run directly in KeepPeek Desktop.
4. `crates/keeppeek-desktop/tests/desktop_contracts.rs` verifies these invariants in CI on Linux, macOS, and Windows.
5. Root `Cargo.toml` patches `urlpattern` 0.3 to `crates/urlpattern-compat` (wrapping maintained `urlpattern` 0.6 / `icu_properties`) and excludes unused Linux GTK3/WebKit2GTK targets (`crates/linux-desktop-unsupported/*`) with compile-time guards so `cargo audit --deny warnings` and GitHub Dependency Review stay clean with zero advisories or warnings.

## Building and packaging locally

### Debug and release binaries

```sh
cargo build --locked -p keeppeek-desktop
cargo build --locked --release -p keeppeek-desktop
```

### Automated tests and lints

```sh
cargo test --locked -p keeppeek-desktop
cargo clippy --locked -p keeppeek-desktop --all-targets -- -D warnings
cargo fmt --all -- --check
```

### Windows binaries, unified installer (NSIS), and Authenticode signing

Official Windows release builds publish three Authenticode-signed, RFC 3161 timestamped binaries (`keeppeek.exe` server with TUI, `keeppeek-service.exe` Windows service, and `keeppeek-desktop.exe` Tauri desktop UI) as standalone `.exe` + `.sha256` assets alongside `keeppeek-<version>-windows-<arch>.zip` and the single unified signed installer `keeppeek-<version>-windows-<arch>-installer.exe` (`installer/keeppeek.nsi`).

`installer/keeppeek.nsi` exposes three selectable components on `MUI_PAGE_COMPONENTS`—**KeepPeek Desktop** (`SecDesktop`), **KeepPeek Server with TUI** (`SecServer`), and **KeepPeek Windows Service** (`SecService`)—and enforces selecting at least one component both interactively (`ComponentsPageLeave`) and silently (`/DESKTOP`, `/SERVER`, `/SERVICE`, `/NO_DESKTOP`, `/NO_SERVER`, `/NO_SERVICE`; zero selected components exits with status `2`).

Requires NSIS 3.13 (`makensis.exe`). For local unsigned builds, `installer/keeppeek.nsi` defaults to single-pass `WriteUninstaller`:

```powershell
cargo build --locked --release -p keeppeek --bin keeppeek --bin keeppeek-service
cargo build --locked --release -p keeppeek-desktop --bin keeppeek-desktop
& 'C:\Program Files (x86)\NSIS\makensis.exe' `
  '/DARCH=x86_64' `
  "/DBUILD_DIR=$PWD\target\release" `
  "/DOUT_DIR=$PWD\target\release" `
  '/DVERSION=0.1.0' `
  '/DVERSION_RESOURCE=0.1.0.0' `
  installer/keeppeek.nsi
```

Release and `Windows Signing` CI workflows use a two-pass NSIS build on `installer/keeppeek.nsi` so no unsigned `uninstall.exe` is written on user machines. On every push to `main`, CI builds `--release` binaries for `windows-x86_64` and `windows-aarch64`, signs all binaries, `uninstall.exe`, and unified installers on `windows-latest`, and publishes them with `.sha256` checksums as `keeppeek-main-<platform>-signed` build artifacts:

1. Compile with `/DWRITE_UNINSTALLER_ONLY` and `/DUNINST_OUT=<path>\uninstall.exe` and execute the temporary generator (`__COMPAT_LAYER=RunAsInvoker`) to extract `uninstall.exe`.
2. Authenticode-sign and RFC 3161 timestamp `uninstall.exe` with Azure Artifact Signing and verify `Get-AuthenticodeSignature` returns `Status == 'Valid'` with a non-null `TimestamperCertificate`.
3. Compile the unified installer with `/DSIGNED_UNINSTALLER=<path>\uninstall.exe` (embedding the signed `uninstall.exe` via `File /oname=uninstall.exe`), then sign and verify `keeppeek-<version>-windows-<arch>-installer.exe`.

### macOS disk image (`.app` and `.dmg`)

Requires macOS on Apple Silicon (`arm64`):

```sh
cargo build --locked --release -p keeppeek-desktop
```

The release workflow packages `KeepPeek Desktop.app` using `installer/macos/desktop/Info.plist`, generates `KeepPeekDesktop.icns` from `assets/readme_logo.jpeg`, signs with Developer ID when secrets are present, creates `keeppeek-desktop-<version>-macos-aarch64.dmg` via `hdiutil`, and notarizes when configured.

Alternatively, developers with `cargo-tauri` installed can run `cargo tauri build` inside `crates/keeppeek-desktop` using `tauri.conf.json`.

## Performance benchmark harness

Use `crates/keeppeek-desktop/scripts/benchmark-desktop.ts` to compare browser Chromium baseline vs. release `keeppeek-desktop` against the same local `keeppeek` server and `test_camera` fixture:

```powershell
bun crates/keeppeek-desktop/scripts/benchmark-desktop.ts --launches 10 --steady-secs 300
```

The script records:

- Cold launch to interactive UI / server load (median and p95 across at least 10 launches)
- Live WebRTC session establishment and browser time-to-first-live-frame (ms)
- Steady-state CPU (%) and total app + webview process memory (MiB) over the configured workload window
- OS, CPU architecture, and WebView2 / Chromium versions

### Recorded Windows baseline and budgets

Measured on Windows 11 (`10.0.26200`, `x64`, AMD Ryzen 5 5600G, 12 logical cores) using release `keeppeek-desktop.exe` (`6.52 MiB`), `WebView2 158.0.4370.33`, and Playwright Chromium `159.0.8154.6` against the same local `keeppeek.exe` + `test_camera.exe` (`cc-4k-640x360-h264.mp4`, 1 camera):

| Metric                                      | Browser baseline (Chromium) | Desktop release (`keeppeek-desktop` + WebView2) |       Delta |          Established budget | Runs / window                                  |
| ------------------------------------------- | --------------------------: | ----------------------------------------------: | ----------: | --------------------------: | ---------------------------------------------- |
| Cold launch to interactive/server load (ms) |    442.5 median / 1,542 p95 |                            633 median / 871 p95 |  +190.5 med | <= 1,000 med / <= 1,500 p95 | 10 cold launches                               |
| Steady-state CPU (%)                        |                      16.03% |                                           1.41% |   -14.62 pp |                   <= 15.00% | 300 s live view after 15 s warmup              |
| Steady-state total process memory (MiB)     |  816.25 median / 822.07 p95 |                      457.89 median / 464.83 p95 | -358.36 med |            <= 650.0 MiB p95 | 52–54 samples over 300 s (all child processes) |
| Live WebRTC session established             |                        true |                                            true |        same |                        true | Observed via `/metrics`                        |

Note: Headless Chromium uses software video decode in Playwright while windowed WebView2 uses host GPU decode, which accounts for the lower desktop CPU usage. The +190.5 ms median cold-launch delta comes from loading the local launcher (median 538 ms) and completing the bounded `GET /auth/session` probe before navigating to the server; p95 launch latency improved by 671 ms. macOS (`WKWebView`) measurements remain pending on physical/CI macOS hardware.
