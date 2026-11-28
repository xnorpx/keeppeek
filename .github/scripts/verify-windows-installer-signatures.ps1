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

function Assert-ValidAuthenticodeSignature {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath
    )

    if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) {
        throw "Required signed binary does not exist: $FilePath"
    }

    $signature = Get-AuthenticodeSignature -LiteralPath $FilePath
    if ($signature.Status -ne 'Valid') {
        throw "$FilePath has invalid Authenticode signature status: $($signature.Status)"
    }
    if ($null -eq $signature.SignerCertificate) {
        throw "$FilePath does not have a signer certificate."
    }
    if ($null -eq $signature.TimestamperCertificate) {
        throw "$FilePath does not have an RFC 3161 timestamp."
    }

    return $signature
}

function Resolve-SevenZipExecutable {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RequestedPath
    )

    $command = Get-Command $RequestedPath -ErrorAction SilentlyContinue
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

    return $executables
}

$sevenZip = Resolve-SevenZipExecutable -RequestedPath $SevenZipPath
$installerSig = Assert-ValidAuthenticodeSignature -FilePath $InstallerPath
$installerHash = Get-LowerSha256 -FilePath $InstallerPath
[Console]::WriteLine("Verified installer: $InstallerPath (SHA256=$installerHash, Signer=$($installerSig.SignerCertificate.Subject))")

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
    $null = Assert-ValidAuthenticodeSignature -FilePath $inputPath
    $expectedHashes[$name] = Get-LowerSha256 -FilePath $inputPath
}

$temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$extractRoot = [System.IO.Path]::Combine(
    $temporaryRoot,
    'keeppeek-installer-verify-' + [guid]::NewGuid().ToString('N')
)

try {
    [void][System.IO.Directory]::CreateDirectory($extractRoot)
    # Extract with -aou so duplicate section copies remain separate files for inspection.
    & $sevenZip x -aou -y "-o$extractRoot" $InstallerPath | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "7-Zip failed to extract $InstallerPath (exit $LASTEXITCODE)."
    }

    $executables = Get-ExtractedExecutables -ExtractRoot $extractRoot
    if ($executables.Count -eq 0) {
        throw "No executables were extracted from $InstallerPath."
    }

    $allowedParents = @(
        [System.IO.Path]::GetFullPath($extractRoot).TrimEnd('\'),
        [System.IO.Path]::GetFullPath((Join-Path $extractRoot '$INSTDIR')).TrimEnd('\')
    )
    $verifiedRequired = @{}

    foreach ($exe in $executables) {
        $parentPath = [System.IO.Path]::GetFullPath($exe.DirectoryName).TrimEnd('\')
        if ($parentPath -notin $allowedParents) {
            throw "Unexpected executable directory inside installer: $($exe.FullName)"
        }

        $lowerName = $exe.Name.ToLowerInvariant()
        if ($expectedHashes.ContainsKey($lowerName)) {
            $canonicalName = $lowerName
        } elseif ($lowerName -match '^(.+)_\d+\.exe$') {
            $canonicalName = "$($Matches[1]).exe"
        } else {
            throw "Unexpected executable extracted from installer: $($exe.Name)"
        }

        if (-not $expectedHashes.ContainsKey($canonicalName)) {
            throw "Unexpected executable extracted from installer: $($exe.Name)"
        }

        $embeddedSig = Assert-ValidAuthenticodeSignature -FilePath $exe.FullName
        $actualHash = Get-LowerSha256 -FilePath $exe.FullName
        $expectedHash = $expectedHashes[$canonicalName]
        if ($actualHash -ne $expectedHash) {
            throw "Embedded $($exe.Name) SHA-256 mismatch: expected $expectedHash, got $actualHash."
        }
        if ($lowerName -eq $canonicalName) {
            $verifiedRequired[$canonicalName] = $true
        }

        [Console]::WriteLine(
            "Verified embedded executable: $($exe.Name) -> $canonicalName (SHA256=$actualHash, Signer=$($embeddedSig.SignerCertificate.Subject))"
        )
    }

    foreach ($requiredName in ($expectedHashes.Keys | Sort-Object)) {
        if (-not $verifiedRequired.ContainsKey($requiredName)) {
            throw "Installer is missing required signed executable: $requiredName"
        }
    }
} finally {
    $resolvedRoot = [System.IO.Path]::GetFullPath($extractRoot)
    if ($resolvedRoot.StartsWith($temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
        [System.IO.Path]::GetFileName($resolvedRoot) -like 'keeppeek-installer-verify-*' -and
        [System.IO.Directory]::Exists($resolvedRoot)) {
        [System.IO.Directory]::Delete($resolvedRoot, $true)
    }
}
