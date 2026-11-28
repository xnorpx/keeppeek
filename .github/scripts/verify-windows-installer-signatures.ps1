param(
    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,
    [Parameter(Mandatory = $true)]
    [string]$SignedBinariesDir,
    [Parameter(Mandatory = $true)]
    [string]$SignedUninstallerPath,
    [string]$SevenZipPath = '7z'
)

$ErrorActionPreference = 'Stop'

$MaxExtractedEntries = 256
$MaxExtractedExecutables = 64
$MaxCleanupEntries = 1024
$SevenZipTimeoutMs = 120000

function Assert-ValidAuthenticodeSignature {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,
        [string]$ExpectedThumbprint = ''
    )

    if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) {
        throw "Required signed binary does not exist: $FilePath"
    }

    $signature = Get-AuthenticodeSignature -LiteralPath $FilePath
    if ($signature.Status -ne 'Valid') {
        throw "$FilePath has invalid Authenticode signature status: $($signature.Status)"
    }
    if ($signature.SignatureType -ne 'Authenticode') {
        throw "$FilePath must have an embedded Authenticode signature, got $($signature.SignatureType)."
    }
    if ($null -eq $signature.SignerCertificate) {
        throw "$FilePath does not have a signer certificate."
    }
    if ($null -eq $signature.TimestamperCertificate) {
        throw "$FilePath does not have an RFC 3161 timestamp."
    }
    if ($ExpectedThumbprint -and
        $signature.SignerCertificate.Thumbprint -ne $ExpectedThumbprint) {
        throw (
            "$FilePath signer thumbprint mismatch: expected $ExpectedThumbprint, " +
            "got $($signature.SignerCertificate.Thumbprint)."
        )
    }

    return $signature
}

function Resolve-SevenZipExecutable {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RequestedPath
    )

    $command = Get-Command $RequestedPath -CommandType Application -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }
    if ($RequestedPath -eq '7z') {
        $defaultPath = Join-Path $env:ProgramFiles '7-Zip\7z.exe'
        if (Test-Path -LiteralPath $defaultPath -PathType Leaf) {
            return $defaultPath
        }
    }

    throw "7-Zip executable was not found: $RequestedPath"
}

function Get-LowerSha256 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath
    )

    return (Get-FileHash -LiteralPath $FilePath -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Remove-DirectorySafely {
    param(
        [Parameter(Mandatory = $true)]
        [string]$TargetRoot,
        [Parameter(Mandatory = $true)]
        [string]$AllowedParent,
        [Parameter(Mandatory = $true)]
        [string]$NamePrefix
    )

    $resolvedRoot = [System.IO.Path]::GetFullPath($TargetRoot)
    if (-not $resolvedRoot.StartsWith($AllowedParent, [System.StringComparison]::OrdinalIgnoreCase) -or
        [System.IO.Path]::GetFileName($resolvedRoot) -notlike "$NamePrefix*" -or
        -not [System.IO.Directory]::Exists($resolvedRoot)) {
        return
    }

    # Unlink reparse points without traversing targets because PowerShell 5.1 Remove-Item follows junctions.
    $pending = [System.Collections.Generic.Queue[System.IO.DirectoryInfo]]::new()
    $pending.Enqueue([System.IO.DirectoryInfo]::new($resolvedRoot))
    $entryCount = 0
    while ($pending.Count -gt 0 -and $entryCount -lt $MaxCleanupEntries) {
        $directory = $pending.Dequeue()
        foreach ($entry in $directory.EnumerateFileSystemInfos()) {
            $entryCount += 1
            if ($entryCount -ge $MaxCleanupEntries) { break }
            if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                if ($entry -is [System.IO.DirectoryInfo]) {
                    [System.IO.Directory]::Delete($entry.FullName, $false)
                } else {
                    [System.IO.File]::Delete($entry.FullName)
                }
                continue
            }
            if ($entry -is [System.IO.DirectoryInfo]) {
                $pending.Enqueue($entry)
            } elseif (($entry.Attributes -band [System.IO.FileAttributes]::ReadOnly) -ne 0) {
                $entry.Attributes = $entry.Attributes -band (-bnot [System.IO.FileAttributes]::ReadOnly)
            }
        }
    }
    if ($pending.Count -gt 0 -or $entryCount -ge $MaxCleanupEntries) {
        return
    }
    [System.IO.Directory]::Delete($resolvedRoot, $true)
}

function Invoke-SevenZipExtract {
    param(
        [Parameter(Mandatory = $true)]
        [string]$SevenZipExecutable,
        [Parameter(Mandatory = $true)]
        [string]$ArchivePath,
        [Parameter(Mandatory = $true)]
        [string]$ExtractRoot
    )

    $extractArgs = "x -tnsis -aou -y `"-o$ExtractRoot`" -- `"$ArchivePath`""
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    if ($SevenZipExecutable -match '\.(cmd|bat)$') {
        $psi.FileName = $env:ComSpec
        $psi.Arguments = "/d /s /c `"`"$SevenZipExecutable`" $extractArgs`""
    } else {
        $psi.FileName = $SevenZipExecutable
        $psi.Arguments = $extractArgs
    }

    $process = [System.Diagnostics.Process]::Start($psi)
    try {
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($SevenZipTimeoutMs)) {
            try { $process.Kill() } catch {}
            throw "7-Zip timed out while extracting $ArchivePath."
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $exitCode = $process.ExitCode
    } finally {
        $process.Dispose()
    }

    $combined = ($stdout + "`n" + $stderr).Trim()
    if ($exitCode -eq 0) {
        return
    }
    # Allow 7-Zip warning exit 1 only for the expected trailing Authenticode certificate table.
    $isTrailingTailWarning = (
        $exitCode -eq 1 -and
        $combined -match 'There are (?:some )?data after the end of (?:archive|the payload data)' -and
        $combined -match 'Warnings:\s*1\b' -and
        $combined -notmatch '\bErrors:\s*[1-9]' -and
        $combined -notmatch 'Sub items Errors:'
    )
    if (-not $isTrailingTailWarning) {
        throw "7-Zip failed to extract $ArchivePath (exit $exitCode): $combined"
    }
}

function Get-ExtractedExecutables {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExtractRoot
    )

    $pending = [System.Collections.Generic.Queue[System.IO.DirectoryInfo]]::new()
    $pending.Enqueue([System.IO.DirectoryInfo]::new($ExtractRoot))
    $executables = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
    $entryCount = 0

    while ($pending.Count -gt 0) {
        $directory = $pending.Dequeue()
        foreach ($entry in $directory.EnumerateFileSystemInfos()) {
            $entryCount += 1
            if ($entryCount -gt $MaxExtractedEntries) {
                throw "Extracted installer entry count exceeds limit ($MaxExtractedEntries)."
            }
            if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Extracted installer contains a reparse point: $($entry.FullName)"
            }
            if ($entry -is [System.IO.DirectoryInfo]) {
                $pending.Enqueue($entry)
                continue
            }
            if ($entry -is [System.IO.FileInfo] -and
                $entry.Extension.Equals('.exe', [System.StringComparison]::OrdinalIgnoreCase)) {
                $executables.Add($entry)
                if ($executables.Count -gt $MaxExtractedExecutables) {
                    throw "Extracted executable count exceeds limit ($MaxExtractedExecutables)."
                }
            }
        }
    }

    return , $executables.ToArray()
}

function Test-ExtractedExecutables {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ArchivePath,
        [Parameter(Mandatory = $true)]
        [string]$ExtractRoot,
        [Parameter(Mandatory = $true)]
        [hashtable]$ExpectedHashes,
        [Parameter(Mandatory = $true)]
        [string]$ExpectedThumbprint
    )

    $executables = Get-ExtractedExecutables -ExtractRoot $ExtractRoot
    if ($executables.Count -eq 0) {
        throw "No executables were extracted from $ArchivePath."
    }

    $allowedParents = @(
        [System.IO.Path]::GetFullPath($ExtractRoot).TrimEnd('\'),
        [System.IO.Path]::GetFullPath((Join-Path $ExtractRoot '$INSTDIR')).TrimEnd('\')
    )
    $verifiedRequired = @{}

    foreach ($exe in $executables) {
        $parentPath = [System.IO.Path]::GetFullPath($exe.DirectoryName).TrimEnd('\')
        if ($parentPath -notin $allowedParents) {
            throw "Unexpected executable directory inside installer: $($exe.FullName)"
        }

        $lowerName = $exe.Name.ToLowerInvariant()
        if ($ExpectedHashes.ContainsKey($lowerName)) {
            $canonicalName = $lowerName
        } elseif ($lowerName -match '^(.+)_\d+\.exe$') {
            $canonicalName = "$($Matches[1]).exe"
        } else {
            throw "Unexpected executable extracted from installer: $($exe.Name)"
        }
        if (-not $ExpectedHashes.ContainsKey($canonicalName)) {
            throw "Unexpected executable extracted from installer: $($exe.Name)"
        }

        $embeddedSig = Assert-ValidAuthenticodeSignature `
            -FilePath $exe.FullName `
            -ExpectedThumbprint $ExpectedThumbprint
        $actualHash = Get-LowerSha256 -FilePath $exe.FullName
        $expectedHash = $ExpectedHashes[$canonicalName]
        if ($actualHash -ne $expectedHash) {
            throw "Embedded $($exe.Name) SHA-256 mismatch: expected $expectedHash, got $actualHash."
        }
        if ($lowerName -eq $canonicalName) {
            $verifiedRequired[$canonicalName] = $true
        }

        $subject = $embeddedSig.SignerCertificate.Subject
        [Console]::WriteLine(
            "Verified embedded executable: $($exe.Name) -> $canonicalName " +
            "(SHA256=$actualHash, Signer=$subject)"
        )
    }

    foreach ($requiredName in ($ExpectedHashes.Keys | Sort-Object)) {
        if (-not $verifiedRequired.ContainsKey($requiredName)) {
            throw "Installer is missing required signed executable: $requiredName"
        }
    }
}

$sevenZip = Resolve-SevenZipExecutable -RequestedPath $SevenZipPath
$installerSig = Assert-ValidAuthenticodeSignature -FilePath $InstallerPath
$expectedThumbprint = $installerSig.SignerCertificate.Thumbprint
$installerHash = Get-LowerSha256 -FilePath $InstallerPath
$installerSubject = $installerSig.SignerCertificate.Subject
[Console]::WriteLine(
    "Verified installer: $InstallerPath (SHA256=$installerHash, Signer=$installerSubject)"
)

$expectedInputs = @{
    'keeppeek.exe'            = Join-Path $SignedBinariesDir 'keeppeek.exe'
    'keeppeek-service.exe'    = Join-Path $SignedBinariesDir 'keeppeek-service.exe'
    'keeppeek-supervisor.exe' = Join-Path $SignedBinariesDir 'keeppeek-supervisor.exe'
    'keeppeek-desktop.exe'    = Join-Path $SignedBinariesDir 'keeppeek-desktop.exe'
    'uninstall.exe'           = $SignedUninstallerPath
}

$expectedHashes = @{}
foreach ($name in ($expectedInputs.Keys | Sort-Object)) {
    $inputPath = $expectedInputs[$name]
    $null = Assert-ValidAuthenticodeSignature `
        -FilePath $inputPath `
        -ExpectedThumbprint $expectedThumbprint
    $expectedHashes[$name] = Get-LowerSha256 -FilePath $inputPath
}

$temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$extractRoot = [System.IO.Path]::Combine(
    $temporaryRoot,
    'keeppeek-installer-verify-' + [guid]::NewGuid().ToString('N')
)

try {
    [void][System.IO.Directory]::CreateDirectory($extractRoot)
    # Extract the installer with -aou so duplicate section copies remain separate files.
    Invoke-SevenZipExtract `
        -SevenZipExecutable $sevenZip `
        -ArchivePath $InstallerPath `
        -ExtractRoot $extractRoot
    Test-ExtractedExecutables `
        -ArchivePath $InstallerPath `
        -ExtractRoot $extractRoot `
        -ExpectedHashes $expectedHashes `
        -ExpectedThumbprint $expectedThumbprint
} finally {
    try {
        Remove-DirectorySafely `
            -TargetRoot $extractRoot `
            -AllowedParent $temporaryRoot `
            -NamePrefix 'keeppeek-installer-verify-'
    } catch {
        # Keep cleanup best-effort so cleanup never masks an extraction or signature error.
    }
}
