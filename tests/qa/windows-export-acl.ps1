<#
.SYNOPSIS
Checks that Windows configuration exports reject inherited access by other users.
.EXAMPLE
powershell -NoProfile -File tests/qa/windows-export-acl.ps1
.NOTES
Uses synthetic ZIP content and a loopback server. A failure records an unfixed QA defect.
#>
[CmdletBinding()]
param(
    [string]$KeepPeekBinary,
    [string]$PythonBinary = 'python',
    [string]$EvidencePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $KeepPeekBinary) { $KeepPeekBinary = Join-Path $PSScriptRoot '../../target/release/keeppeek.exe' }
if (-not $EvidencePath) { $EvidencePath = Join-Path $PSScriptRoot '../../target/alpha-audit/windows-export-acl.json' }
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This test requires Windows filesystem ACLs.'
}
$KeepPeekBinary = (Resolve-Path -LiteralPath $KeepPeekBinary).Path
$PythonBinary = (Get-Command $PythonBinary -ErrorAction Stop).Source
$workRoot = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp'))
$workDirectory = Join-Path $workRoot ('keeppeek-export-acl-' + [guid]::NewGuid().ToString('N'))
$serverProcess = $null
$exportProcess = $null

function Start-SyntheticZipServer([string]$Directory, [string]$Python) {
    $scriptPath = Join-Path $Directory 'serve.py'
    @'
from http.server import HTTPServer, BaseHTTPRequestHandler
from io import BytesIO
from pathlib import Path
import zipfile

buffer = BytesIO()
with zipfile.ZipFile(buffer, "w") as archive:
    archive.writestr("config.toml", "port = 3000\n")
    archive.writestr("secrets.toml", 'QA_ONLY = "synthetic-placeholder"\n')
payload = buffer.getvalue()

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/zip")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_):
        pass

server = HTTPServer(("127.0.0.1", 0), Handler)
server.timeout = 30
Path(__file__).with_suffix(".port").write_text(str(server.server_port))
server.handle_request()
server.server_close()
'@ | Set-Content -LiteralPath $scriptPath -Encoding utf8
    Start-Process -FilePath $Python -ArgumentList @('"' + $scriptPath + '"') `
        -WindowStyle Hidden -PassThru
}

function Get-OtherUserReadRules([string]$Path) {
    $broadReaders = @('S-1-1-0', 'S-1-5-11', 'S-1-5-32-545')
    @((Get-Acl -LiteralPath $Path).Access | Where-Object {
        $sid = $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
        $broadReaders -contains $sid -and
            $_.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
            ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::ReadData) -ne 0
    } | ForEach-Object {
        [ordered]@{
            sid = $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
            rights = $_.FileSystemRights.ToString()
            inherited = $_.IsInherited
        }
    })
}

try {
    New-Item -ItemType Directory -Path $workDirectory | Out-Null
    $directoryAcl = Get-Acl -LiteralPath $workDirectory
    $readers = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-545')
    $readRule = [Security.AccessControl.FileSystemAccessRule]::new(
        $readers, [Security.AccessControl.FileSystemRights]::ReadAndExecute,
        [Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit',
        [Security.AccessControl.PropagationFlags]::None,
        [Security.AccessControl.AccessControlType]::Allow
    )
    $directoryAcl.AddAccessRule($readRule)
    Set-Acl -LiteralPath $workDirectory -AclObject $directoryAcl
    if (@(Get-OtherUserReadRules $workDirectory).Count -eq 0) {
        throw 'Fixture setup failed: the destination must inherit a broad read permission.'
    }
    $serverProcess = Start-SyntheticZipServer $workDirectory $PythonBinary
    $portPath = Join-Path $workDirectory 'serve.port'
    for ($attempt = 0; $attempt -lt 100 -and -not (Test-Path -LiteralPath $portPath); $attempt++) {
        if ($serverProcess.HasExited) { throw 'The synthetic ZIP server exited before readiness.' }
        Start-Sleep -Milliseconds 100
    }
    if (-not (Test-Path -LiteralPath $portPath)) { throw 'The synthetic ZIP server did not become ready.' }
    $port = [int](Get-Content -LiteralPath $portPath)
    $outputPath = Join-Path $workDirectory 'configuration.zip'
    $stdoutPath = Join-Path $workDirectory 'export.stdout'
    $exportArguments = 'config --server http://127.0.0.1:{0} export --output "{1}"' -f $port, $outputPath
    $exportProcess = Start-Process -FilePath $KeepPeekBinary -ArgumentList $exportArguments `
        -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError (Join-Path $workDirectory 'export.stderr')
    # Retain the handle so Windows PowerShell 5.1 preserves ExitCode after WaitForExit.
    $null = $exportProcess.Handle
    if (-not $exportProcess.WaitForExit(20000)) { throw 'Configuration export exceeded the 20-second test budget.' }
    if ($exportProcess.ExitCode -ne 0) { throw 'Configuration export failed before the ACL assertion.' }
    $exported = Get-Content -LiteralPath $stdoutPath -Raw | ConvertFrom-Json
    if ([int64]$exported.archiveBytes -ne (Get-Item -LiteralPath $outputPath).Length) {
        throw 'Configuration export did not save the complete synthetic ZIP.'
    }
    $unsafeRules = @(Get-OtherUserReadRules $outputPath)
    $evidence = [ordered]@{
        test = 'Windows configuration exports exclude broad inherited read access'
        timestampUtc = [DateTime]::UtcNow.ToString('o')
        binarySha256 = (Get-FileHash -LiteralPath $KeepPeekBinary -Algorithm SHA256).Hash.ToLowerInvariant()
        archiveBytes = [int64]$exported.archiveBytes
        exitCode = $exportProcess.ExitCode
        passed = $unsafeRules.Count -eq 0
        unexpectedReadRules = $unsafeRules
    }
    $EvidencePath = [IO.Path]::GetFullPath($EvidencePath)
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $EvidencePath) | Out-Null
    $evidence | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $EvidencePath -Encoding utf8
    if ($unsafeRules.Count -gt 0) {
        throw 'Exported plaintext secrets remain readable by other local users through Windows ACLs.'
    }
    Write-Output 'PASS: Configuration export excludes broad inherited read access.'
} finally {
    foreach ($process in @($exportProcess, $serverProcess)) {
        if ($null -ne $process -and -not $process.HasExited) {
            $process.Kill()
            if (-not $process.WaitForExit(5000)) { throw 'A test-owned process did not stop.' }
        }
    }
    if (Test-Path -LiteralPath $workDirectory) {
        $resolvedWork = (Resolve-Path -LiteralPath $workDirectory).Path
        if (-not $resolvedWork.StartsWith($workRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Refusing cleanup outside the test temporary root.'
        }
        Remove-Item -LiteralPath $resolvedWork -Recurse -Force
    }
}
