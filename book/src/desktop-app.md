# Desktop application (Windows and macOS)

KeepPeek Desktop packages the existing KeepPeek web interface inside a native Tauri v2 window for Windows and macOS. It lets you open KeepPeek from the Start Menu, Applications folder, or Dock without managing browser tabs.

## Backend deployment model

KeepPeek Desktop is a **connect-to-existing-server client**. It does not bundle, spawn, own, or stop a KeepPeek server process.

| Concern              | Owner                                                                                              |
| -------------------- | -------------------------------------------------------------------------------------------------- |
| Recorder startup     | Operator or OS service (`keeppeek`, Windows `KeepPeekService`, macOS `com.keeppeek`, or Docker).   |
| Recorder shutdown    | Operator or OS service manager. Closing or quitting KeepPeek Desktop never stops recording.        |
| Desktop window & UI  | `keeppeek-desktop` Tauri application and OS webview (`WebView2` on Windows, `WKWebView` on macOS). |
| Server settings & DB | Existing KeepPeek server (`config.toml`, `secrets.toml`, `recordings.db`, media directories).      |
| Desktop default URL  | Stored in the existing `config.toml` under `[desktop].server_url`. No separate settings file.      |

This separation ensures a 24/7 recorder keeps capturing footage after you close the desktop window, and lets one laptop connect to a local or remote KeepPeek server.

## Supported platforms and webviews

| Operating system | Minimum version               | CPU architectures         | Native webview engine              | Release artifacts                                                                                  |
| ---------------- | ----------------------------- | ------------------------- | ---------------------------------- | -------------------------------------------------------------------------------------------------- |
| Windows          | Windows 10 1809+ / Windows 11 | `x86_64`, `aarch64`       | Microsoft Edge WebView2 (Chromium) | `keeppeek-<version>-windows-<arch>-installer.exe`, `keeppeek-desktop-<version>-windows-<arch>.exe` |
| macOS            | macOS 11 (Big Sur)+           | `aarch64` (Apple Silicon) | System `WKWebView` (WebKit)        | `keeppeek-desktop-<version>-macos-aarch64.dmg`                                                     |

Linux and mobile desktop builds are intentionally out of scope. Use a browser on those platforms.

## Installation and uninstall

### Windows

Official Windows releases publish three Authenticode-signed, RFC 3161 timestamped standalone binaries—`keeppeek-<version>-windows-<arch>.exe` (server with TUI), `keeppeek-service-<version>-windows-<arch>.exe` (Windows service), and `keeppeek-desktop-<version>-windows-<arch>.exe` (Tauri desktop UI)—plus a single unified signed installer (`keeppeek-<version>-windows-<arch>-installer.exe`) and the server archive (`keeppeek-<version>-windows-<arch>.zip`).

In the unified installer, choose at least one component:

- **KeepPeek Desktop** (`keeppeek-desktop.exe` and Start Menu shortcut)
- **KeepPeek Server with TUI** (`keeppeek.exe`)
- **KeepPeek Windows Service** (`keeppeek-service.exe` and `KeepPeekService`)

Every executable installed by `keeppeek-<version>-windows-<arch>-installer.exe`, including `uninstall.exe`, is Authenticode-signed and RFC 3161 timestamped in official releases.

1. Download `keeppeek-<version>-windows-<arch>-installer.exe` (or the standalone `keeppeek-desktop-<version>-windows-<arch>.exe`) and its `.sha256` file from the [release page](https://github.com/xnorpx/keeppeek/releases).
2. Verify the checksum and signature in PowerShell:

```powershell
Get-FileHash keeppeek-<version>-windows-x86_64-installer.exe -Algorithm SHA256
Get-AuthenticodeSignature keeppeek-<version>-windows-x86_64-installer.exe
```

3. Run the unified installer and select **KeepPeek Desktop** (plus **KeepPeek Server with TUI** and/or **KeepPeek Windows Service** if you also want to run the recorder on this machine). For unattended installs, combine `/S` with `/DESKTOP`, `/SERVER`, `/SERVICE` or `/NO_DESKTOP`, `/NO_SERVER`, `/NO_SERVICE` (at least one component must remain selected; e.g. `/S /DESKTOP /NO_SERVER /NO_SERVICE`). If you prefer a portable launch without administrator installation, run the signed standalone `keeppeek-desktop-<version>-windows-<arch>.exe` directly.
4. Launch **KeepPeek Desktop** from the Start Menu.
5. To uninstall, use **Installed apps** in Windows Settings or run the signed `C:\Program Files\KeepPeek\uninstall.exe`. Uninstalling removes the installed KeepPeek binaries, shortcuts, and `KeepPeekService` if present, while leaving `%APPDATA%\keeppeek\config.toml` untouched.

Windows 11 includes the Microsoft Edge WebView2 Runtime. On older Windows 10 installations, install the Evergreen WebView2 Runtime if prompted.

### macOS

1. Download `keeppeek-desktop-<version>-macos-aarch64.dmg` and its matching `.sha256` file.
2. Verify the checksum:

```sh
shasum -a 256 -c keeppeek-desktop-<version>-macos-aarch64.dmg.sha256
```

3. Open the disk image and drag `KeepPeek Desktop.app` to `/Applications`.
4. Launch `KeepPeek Desktop.app`.
5. To uninstall, quit **KeepPeek Desktop** (`Cmd+Q`) and delete `/Applications/KeepPeek Desktop.app`. This leaves `KeepPeek.app`, the `com.keeppeek` LaunchAgent, and `~/Library/Application Support/keeppeek/config.toml` untouched.

## First launch and server connection

On startup, KeepPeek Desktop resolves its target server URL in this order:

1. `--server-url <URL>` command-line flag
2. `KEEPPEEK_DESKTOP_SERVER_URL` environment variable
3. `[desktop].server_url` in `config.toml`
4. Derived local address `http://127.0.0.1:<port>` from root `host` and `port` in `config.toml`
5. Default `http://127.0.0.1:8081`

Before opening the server UI, KeepPeek Desktop sends a bounded `GET /auth/session` probe (5-second timeout, redirects disabled, 64 KiB response limit) to verify that a KeepPeek server is reachable.

- When the probe succeeds, the window navigates directly to the server's SvelteKit interface. Trusted-local administrator access, bearer access-key sign-in, same-origin reverse-proxy sessions, live dashboard, recorded playback, events, health, and authorized settings run directly from the server.
- Cross-origin OIDC login redirects (`POST /auth/login` to an external identity provider) and popup-based administrator OIDC verification (`window.open`) are intentionally blocked by the desktop webview's single-origin navigation and popup restrictions. Use a standard browser for OIDC sign-in or administrator OIDC verification.
- When the server is unreachable, times out, fails TLS verification, requires HTTPS (`HTTP 426`), or returns a non-KeepPeek response, the launcher stays open and displays the exact failure with a recovery action (**Retry** or update the server URL).
- To switch servers at any time, choose **Connection → Switch Server...** (`Ctrl+Shift+S` on Windows, `Cmd+Shift+S` on macOS).
- Use **Connection → Reconnect** (`Ctrl+Shift+R` / `Cmd+Shift+R`) to re-probe and reconnect, or **Connection → Reload** (`Ctrl+R` / `Cmd+R`) to refresh the current page.

Use `--no-autoconnect` to open the launcher without auto-connecting, and `--config <path>` (or `-c <path>`) to select an alternate `config.toml`.

## Window close, reopen, and exit

- **Windows:** Closing the window exits `keeppeek-desktop.exe` and releases its WebView2 child processes. Any running `keeppeek.exe` or `KeepPeekService` instance continues unaffected.
- **macOS:** Closing the window navigates away from active media sessions (triggering `pagehide` so WebRTC peers close cleanly) and hides the window; clicking the Dock icon reopens it. Choosing **Quit KeepPeek Desktop** (`Cmd+Q`) exits the client while leaving `com.keeppeek` running.

## Security boundaries

- **Zero native capabilities for server content:** Tauri capabilities (`crates/keeppeek-desktop/capabilities/default.json`) are restricted to `"local": true` and `withGlobalTauri: false`. Once the webview navigates to a KeepPeek server origin, server pages and scripts cannot invoke native Tauri commands.
- **Strict launcher CSP:** The bundled launcher uses `default-src 'self'` with no inline scripts, no inline styles, and no wildcards.
- **Navigation and popup restrictions:** The Rust `on_navigation` guard permits only the bundled launcher origin and the active verified KeepPeek server origin. Disallowed schemes (`file:`, `javascript:`, `data:`), embedded credentials, third-party origins, and popup windows (`target="_blank"` / `window.open`) are blocked.
- **Server-enforced access control:** Local/remote classification, session cookies, CSRF tokens, bearer credentials, camera grants, and privacy schedules remain enforced exclusively by the KeepPeek server.

## Media, codecs, and accessibility

KeepPeek streams camera media without transcoding, so playback depends on the OS webview:

- **Windows (`WebView2` / Chromium):** Supports standard WebRTC and H.264 playback matching Microsoft Edge on the host. Hardware-backed HEVC/H.265 decoding depends on installed Windows video extensions and GPU support.
- **macOS (`WKWebView` / WebKit):** Uses Apple's WebKit media stack and VideoToolbox decoders. Codec behavior can differ from Chromium for non-standard H.264 profiles or high-bitrate streams; retain a broadly compatible H.264 substream for reliable live view and playback.

Unsupported codec states remain visibly reported by the existing KeepPeek UI rather than failing silently. Both the bundled launcher and the loaded KeepPeek UI preserve keyboard navigation, visible focus outlines, semantic landmarks, and `role="alert"` / `role="status"` announcements.

## Build from source

See the [desktop engineering guide](https://github.com/xnorpx/keeppeek/blob/main/docs/desktop-app.md) for local build commands, packaging scripts, automated contract tests, and benchmark instructions.
