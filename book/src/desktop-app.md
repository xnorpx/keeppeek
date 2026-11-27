# Desktop application (Windows and macOS)

KeepPeek Desktop (`crates/keeppeek-desktop`) packages the KeepPeek web interface in a native Tauri v2 window for Windows and macOS so you can connect to an existing local or remote KeepPeek server without managing browser tabs. It is a client only—it never bundles, starts, or stops a recorder—and cross-origin OIDC sign-in or popup administrator verification requires a standard browser.

## Deployment model

| Concern               | Owner                                                                                            |
| --------------------- | ------------------------------------------------------------------------------------------------ |
| Recorder startup      | Operator or OS service (`keeppeek`, Windows `KeepPeekService`, macOS `com.keeppeek`, or Docker). |
| Recorder shutdown     | Operator or OS service manager. Closing or quitting KeepPeek Desktop never stops recording.      |
| Desktop window and UI | `keeppeek-desktop` Tauri shell + OS webview (`WebView2` on Windows, `WKWebView` on macOS).       |
| Server config and DB  | Target KeepPeek server (`config.toml`, `secrets.toml`, `recordings.db`, media directories).      |
| Saved server URL      | `[desktop].server_url` in the existing canonical `config.toml` (no separate settings file).      |

Keeping the desktop client decoupled lets a 24/7 recorder continue capturing footage after you close the window and lets one laptop switch between local and remote recorders.

## Supported platforms and artifacts

| OS      | Minimum version               | Architectures             | Native webview                     | Release artifacts                                                                                  |
| ------- | ----------------------------- | ------------------------- | ---------------------------------- | -------------------------------------------------------------------------------------------------- |
| Windows | Windows 10 1809+ / Windows 11 | `x86_64`, `aarch64`       | Microsoft Edge WebView2 (Chromium) | `keeppeek-<version>-windows-<arch>-installer.exe`, `keeppeek-desktop-<version>-windows-<arch>.exe` |
| macOS   | macOS 11 (Big Sur)+           | `aarch64` (Apple Silicon) | System `WKWebView` (WebKit)        | `keeppeek-desktop-<version>-macos-aarch64.dmg`                                                     |

Linux and mobile desktop builds are out of scope; use a standard browser on those platforms.

## Installation and uninstall

### Windows

Official Windows releases publish Authenticode-signed, RFC 3161 timestamped executables (`keeppeek.exe`, `keeppeek-service.exe`, `keeppeek-desktop.exe`, `uninstall.exe`, and the unified installer), each with a matching `.sha256` file.

1. Download `keeppeek-<version>-windows-<arch>-installer.exe` (or standalone `keeppeek-desktop-<version>-windows-<arch>.exe` for portable use) and its `.sha256` file from the [releases page](https://github.com/xnorpx/keeppeek/releases).
2. Verify checksum and signature in PowerShell:

```powershell
Get-FileHash keeppeek-<version>-windows-x86_64-installer.exe -Algorithm SHA256
Get-AuthenticodeSignature keeppeek-<version>-windows-x86_64-installer.exe
```

3. Run the installer and select **KeepPeek Desktop** (optionally also **KeepPeek Server with TUI** and/or **KeepPeek Windows Service**). For unattended installs, combine `/S` with `/DESKTOP`, `/SERVER`, `/SERVICE`, `/NO_DESKTOP`, `/NO_SERVER`, or `/NO_SERVICE`—at least one component must stay selected (for example, `/S /DESKTOP /NO_SERVER /NO_SERVICE`).
4. Launch **KeepPeek Desktop** from the Start Menu. On older Windows 10 builds without WebView2, install the Microsoft Edge Evergreen WebView2 Runtime if prompted.
5. Uninstall via Windows **Installed apps** or signed `C:\Program Files\KeepPeek\uninstall.exe`. Uninstalling removes binaries, shortcuts, and `KeepPeekService` if present, while preserving `%APPDATA%\keeppeek\config.toml`.

### macOS

1. Download `keeppeek-desktop-<version>-macos-aarch64.dmg` and its `.sha256` file, then verify:

```sh
shasum -a 256 -c keeppeek-desktop-<version>-macos-aarch64.dmg.sha256
```

2. Open the DMG, drag `KeepPeek Desktop.app` to `/Applications`, and launch it. Signing and notarization depend on the release workflow's credentials; verify the downloaded artifact.
3. Uninstall by quitting (`Cmd+Q`) and deleting `/Applications/KeepPeek Desktop.app`. This leaves `KeepPeek.app`, `com.keeppeek`, and `~/Library/Application Support/keeppeek/config.toml` untouched.

## Connecting to a server

### URL resolution precedence

On startup, `keeppeek-desktop` resolves its target origin in this order:

1. `--server-url <URL>` CLI flag
2. `KEEPPEEK_DESKTOP_SERVER_URL` environment variable
3. `[desktop].server_url` in `config.toml` (see [Desktop client settings](./configuration-reference.md#desktop-client-settings))
4. Derived local origin `http://127.0.0.1:<port>` from root `host` and `port` in `config.toml`
5. Fallback `http://127.0.0.1:8081`

| CLI flag                | Purpose                                                               |
| ----------------------- | --------------------------------------------------------------------- |
| `--server-url <URL>`    | Connect to a specific canonical `http://` or `https://` server origin |
| `--no-autoconnect`      | Open the launcher screen instead of auto-connecting on startup        |
| `-c`, `--config <PATH>` | Read and write `[desktop].server_url` in an alternate `config.toml`   |

### Pre-flight probe and launcher controls

Before loading the server UI, the launcher executes a bounded `GET /auth/session` probe (5-second timeout, 0 redirects, 64 KiB response cap, max 64 advertised auth methods).

- **On success:** The webview navigates directly to `<origin>/`. Trusted-local Administrator access, bearer access-key sign-in, and same-origin identity-proxy sessions run directly from the server.
- **On failure:** The launcher stays open with an actionable error message, allowing you to edit **Server URL**, toggle **Save as default in `config.toml`**, and choose **Connect** or **Retry**.

| Failure state          | Cause and recovery                                                                          |
| ---------------------- | ------------------------------------------------------------------------------------------- |
| Unreachable / timeout  | Server stopped, wrong port, or VPN/network blocked. Start the server and click **Retry**.   |
| TLS verification error | Untrusted certificate. Install/trust the certificate in the OS trust store, then retry.     |
| `HTTP 426`             | Server enforces HTTPS for remote access. Use an `https://` origin or a trusted local host.  |
| Redirect / non-server  | Endpoint redirected or did not return KeepPeek session JSON. Enter the direct server URL.   |
| OIDC redirect / popup  | Blocked by single-origin and no-popup rules. Use a standard browser for OIDC sign-in/proof. |

### Menu actions and shortcuts

| Menu item                              | Windows        | macOS         | Action                                      |
| -------------------------------------- | -------------- | ------------- | ------------------------------------------- |
| **Connection → Switch Server...**      | `Ctrl+Shift+S` | `Cmd+Shift+S` | Return to the launcher without auto-connect |
| **Connection → Reconnect**             | `Ctrl+Shift+R` | `Cmd+Shift+R` | Re-probe the active origin and reconnect    |
| **Connection → Reload**                | `Ctrl+R`       | `Cmd+R`       | Reload the current server page              |
| **Connection → Quit KeepPeek Desktop** | `Ctrl+Q`       | `Cmd+Q`       | Exit the client (recorder keeps running)    |

## Window close, reopen, and exit

- **Windows:** Closing the window exits `keeppeek-desktop.exe` and releases WebView2 child processes; `keeppeek.exe` and `KeepPeekService` keep running.
- **macOS:** Closing the window navigates back to the launcher (triggering `pagehide` so WebRTC peers close cleanly) and hides the window. Clicking the Dock icon reopens and reconnects. Choosing **Quit KeepPeek Desktop** (`Cmd+Q`) exits the client while `com.keeppeek` continues running.

## Security boundaries and playback limits

- **Zero native access for server pages:** `crates/keeppeek-desktop/capabilities/default.json` restricts capabilities to `"local": true` on the `"main"` window with `"withGlobalTauri": false`. Once navigated to a server origin, remote pages cannot call native Tauri commands.
- **Strict launcher CSP:** `default-src 'self'` with no inline scripts, inline styles, `eval`, or wildcards.
- **Single-origin navigation and no popups:** The Rust `on_navigation` guard permits only the bundled launcher origin (`tauri://localhost`, `http(s)://tauri.localhost`) and the verified server origin. Disallowed schemes (`file:`, `javascript:`, `data:`), embedded credentials, third-party origins, and all popup windows (`target="_blank"`, `window.open`) are blocked.
- **Server-enforced policy:** Local/remote classification, cookies, CSRF tokens, bearer keys, camera grants, and privacy schedules remain enforced exclusively by the KeepPeek server.
- **Webview codec behavior:** KeepPeek streams without transcoding. Windows `WebView2` matches Edge (H.264 standard; HEVC depends on OS video extensions and GPU). macOS `WKWebView` uses WebKit/VideoToolbox, which can reject non-standard H.264 profiles or high bitrates; retain a compatible H.264 substream. Unsupported codecs remain visibly reported by the UI. Both launcher and server UI preserve keyboard navigation, focus outlines, landmarks, and `role="alert"` / `role="status"` announcements.

For local builds, signing scripts, automated contract tests, and recorded Windows benchmarks, see the [desktop engineering guide](https://github.com/xnorpx/keeppeek/blob/main/docs/desktop-app.md).
