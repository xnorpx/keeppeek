# Windows service

`keeppeek-service.exe` runs KeepPeek through the Windows Service Control Manager. Its service name is `KeepPeekService`, so another Windows service or an orchestration tool can start and stop it through the standard SCM APIs.

Install it from an elevated prompt:

```powershell
sc.exe create KeepPeekService binPath= "C:\path\to\keeppeek-service.exe" start= auto
sc.exe start KeepPeekService
```

The service reads the normal KeepPeek configuration. It defaults to `[logging] service = "file"`, which appends logs to `%APPDATA%\keeppeek\keeppeek-service.log`; set it to `"event_log"` to send logs to the Windows Application event log. Before using `event_log`, create the source once from an elevated PowerShell prompt:

```powershell
New-EventLog -LogName Application -Source KeepPeekService
```

Stop and remove the service with `sc.exe stop KeepPeekService` and `sc.exe delete KeepPeekService`.
