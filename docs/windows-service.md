# Windows service

`keeppeek-service.exe` runs KeepPeek through the Windows Service Control Manager as a slim parent supervisor. Its service name is `KeepPeekService`, so another Windows service or an orchestration tool can start and stop it through the standard SCM APIs.

At runtime, `keeppeek-service.exe` embeds `crates/keeppeek-supervisor` to launch `keeppeek.exe` as a separate child process inside a Win32 Job Object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). It resolves content-addressed A/B executable slots under `<config_dir>\supervisor\versions\<slot_id>\keeppeek.exe` (enforcing owner-only ACLs on the supervisor root before launch) and falls back to the sibling `keeppeek.exe` installed alongside `keeppeek-service.exe` when no A/B slot has been staged yet. If a staged `keeppeek-vision.exe` slot or sibling binary exists, the supervisor manages it as an independent child process without interrupting core recording.

To test local developer builds under the Windows service, an administrator can explicitly pass `--allow-local` (or `--allow-unsigned-main`) in the service `binPath` ImagePath arguments and then drive live swaps with `keeppeek-supervisor update --root <config_dir>\supervisor --channel local --binary <path>\keeppeek.exe --apply --wait`. Neither flag is ever persisted in `config.toml`.

The Windows installer includes `keeppeek.exe`, `keeppeek-service.exe`, and `keeppeek-supervisor.exe` (selecting the service component installs all three). Select **KeepPeek Windows Service** during installation to register and start the service; during installer-based upgrades the installer stops and removes any existing service before replacing binaries, then recreates and starts it. Its uninstaller stops and removes the service.

Install it from an elevated prompt:

```powershell
sc.exe create KeepPeekService binPath= "C:\path\to\keeppeek-service.exe" start= auto
sc.exe failure KeepPeekService reset= 86400 actions= restart/5000/restart/15000/restart/30000
sc.exe failureflag KeepPeekService 1
sc.exe start KeepPeekService
```

The service reads the normal KeepPeek configuration. It defaults to `[logging] service = "file"`, which appends logs to `%APPDATA%\keeppeek\keeppeek-service.log`; set it to `"event_log"` to send logs to the Windows Application event log. Before using `event_log`, create the source once from an elevated PowerShell prompt:

```powershell
New-EventLog -LogName Application -Source KeepPeekService
```

Stop and remove the service with `sc.exe stop KeepPeekService` and `sc.exe delete KeepPeekService`.
