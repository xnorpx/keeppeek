$ErrorActionPreference = 'Stop'

function Resolve-SignedFixtureExecutable {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Candidates
    )

    foreach ($candidate in $Candidates) {
        if (-not $candidate -or -not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            continue
        }
        $signature = Get-AuthenticodeSignature -LiteralPath $candidate
        if ($signature.Status -eq 'Valid' -and
            $null -ne $signature.SignerCertificate -and
            $null -ne $signature.TimestamperCertificate) {
            return $candidate
        }
    }

    throw "No signed, RFC 3161 timestamped executable was found among: $($Candidates -join ', ')"
}

function Assert-VerificationFails {
    param(
        [Parameter(Mandatory = $true)]
        [string]$CaseName,
        [Parameter(Mandatory = $true)]
        [string]$ExpectedPattern,
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action
    )

    $failed = $false
    try {
        & $Action
    } catch {
        $failed = $true
        $message = $_.Exception.Message
        if ($message -notmatch $ExpectedPattern) {
            throw "Case '$CaseName' failed with unexpected error: $message"
        }
    }
    if (-not $failed) {
        throw "Case '$CaseName' succeeded unexpectedly."
    }
}

$verifyScript = [System.IO.Path]::Combine($PSScriptRoot, 'verify-windows-installer-signatures.ps1')
$signedPrimary = Resolve-SignedFixtureExecutable -Candidates @(
    'C:\Program Files\Git\bin\git.exe',
    'C:\Program Files\PowerShell\7\pwsh.exe',
    'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
)
$signedAlternate = Resolve-SignedFixtureExecutable -Candidates @(
    'C:\Windows\System32\sc.exe',
    'C:\Windows\System32\cmd.exe',
    'C:\Windows\System32\where.exe'
)
if ((Get-FileHash -LiteralPath $signedPrimary -Algorithm SHA256).Hash -eq
    (Get-FileHash -LiteralPath $signedAlternate -Algorithm SHA256).Hash) {
    throw 'Signed test fixtures must have distinct SHA-256 hashes.'
}

$temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$testRoot = [System.IO.Path]::Combine(
    $temporaryRoot,
    'keeppeek-sigcheck-test-' + [guid]::NewGuid().ToString('N')
)
$previousSrc = $env:KEEPPEEK_FAKE_7Z_SRC
$previousExit = $env:KEEPPEEK_FAKE_7Z_EXIT

try {
    $signedDir = Join-Path $testRoot 'signed'
    $payloadDir = Join-Path $testRoot 'payload'
    $pluginsDir = Join-Path $payloadDir '$PLUGINSDIR'
    foreach ($dir in $signedDir, $payloadDir, $pluginsDir) {
        [void][System.IO.Directory]::CreateDirectory($dir)
    }

    $installerExe = Join-Path $testRoot 'keeppeek-master-windows-x86_64-installer.exe'
    $signedUninstaller = Join-Path $testRoot 'uninstall.exe'
    $unsignedExe = Join-Path $testRoot 'unsigned.exe'
    Copy-Item -LiteralPath $signedPrimary -Destination $installerExe
    Copy-Item -LiteralPath $signedPrimary -Destination $signedUninstaller
    [System.IO.File]::WriteAllText($unsignedExe, 'not-a-signed-pe')

    $binaryNames = @(
        'keeppeek.exe',
        'keeppeek-service.exe',
        'keeppeek-supervisor.exe',
        'keeppeek-desktop.exe'
    )
    foreach ($name in $binaryNames) {
        Copy-Item -LiteralPath $signedPrimary -Destination (Join-Path $signedDir $name)
        Copy-Item -LiteralPath $signedPrimary -Destination (Join-Path $payloadDir $name)
    }
    Copy-Item -LiteralPath $signedUninstaller -Destination (Join-Path $payloadDir 'uninstall.exe')
    # Include a duplicate section copy and a non-executable NSIS plugin.
    Copy-Item -LiteralPath $signedPrimary -Destination (Join-Path $payloadDir 'keeppeek_1.exe')
    [System.IO.File]::WriteAllText((Join-Path $pluginsDir 'nsExec.dll'), 'plugin-stub')

    $fakeSevenZip = Join-Path $testRoot 'fake-7z.cmd'
    $fakeCmd = @(
        '@echo off',
        'if not "%KEEPPEEK_FAKE_7Z_EXIT%"=="" exit /b %KEEPPEEK_FAKE_7Z_EXIT%',
        'set "OUT=%~4"',
        'set "OUT=%OUT:~2%"',
        'xcopy /E /I /Y /Q "%KEEPPEEK_FAKE_7Z_SRC%\*" "%OUT%\" >nul',
        'exit /b 0'
    ) -join "`r`n"
    [System.IO.File]::WriteAllText($fakeSevenZip, $fakeCmd)

    $env:KEEPPEEK_FAKE_7Z_SRC = $payloadDir
    $env:KEEPPEEK_FAKE_7Z_EXIT = ''

    # Positive case: signed installer, all 5 signed binaries, duplicate copy, and NSIS plugin.
    & $verifyScript `
        -InstallerPath $installerExe `
        -SignedBinariesDir $signedDir `
        -SignedUninstallerPath $signedUninstaller `
        -SevenZipPath $fakeSevenZip

    # Negative case 1: unsigned outer installer fails before extraction.
    Assert-VerificationFails -CaseName 'unsigned-installer' -ExpectedPattern 'invalid Authenticode signature status' -Action {
        & $verifyScript `
            -InstallerPath $unsignedExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }

    # Negative case 2: unsigned embedded uninstaller fails verification.
    Copy-Item -LiteralPath $unsignedExe -Destination (Join-Path $payloadDir 'uninstall.exe') -Force
    Assert-VerificationFails -CaseName 'unsigned-embedded-uninstaller' -ExpectedPattern 'invalid Authenticode signature status' -Action {
        & $verifyScript `
            -InstallerPath $installerExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }
    Copy-Item -LiteralPath $signedUninstaller -Destination (Join-Path $payloadDir 'uninstall.exe') -Force

    # Negative case 3: signed embedded binary with wrong SHA-256 fails verification.
    Copy-Item -LiteralPath $signedAlternate -Destination (Join-Path $payloadDir 'keeppeek.exe') -Force
    Assert-VerificationFails -CaseName 'hash-mismatch' -ExpectedPattern 'SHA-256 mismatch' -Action {
        & $verifyScript `
            -InstallerPath $installerExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }
    Copy-Item -LiteralPath $signedPrimary -Destination (Join-Path $payloadDir 'keeppeek.exe') -Force

    # Negative case 4: missing required binary fails verification.
    Remove-Item -LiteralPath (Join-Path $payloadDir 'keeppeek-supervisor.exe')
    Assert-VerificationFails -CaseName 'missing-supervisor' -ExpectedPattern 'missing required signed executable: keeppeek-supervisor\.exe' -Action {
        & $verifyScript `
            -InstallerPath $installerExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }
    Copy-Item -LiteralPath $signedPrimary -Destination (Join-Path $payloadDir 'keeppeek-supervisor.exe') -Force

    # Negative case 5: unexpected extra executable fails verification.
    Copy-Item -LiteralPath $signedPrimary -Destination (Join-Path $payloadDir 'extra.exe')
    Assert-VerificationFails -CaseName 'unexpected-exe' -ExpectedPattern 'Unexpected executable extracted from installer' -Action {
        & $verifyScript `
            -InstallerPath $installerExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }
    Remove-Item -LiteralPath (Join-Path $payloadDir 'extra.exe')

    # Negative case 6: executable inside unexpected subdirectory fails verification.
    Copy-Item -LiteralPath $signedPrimary -Destination (Join-Path $pluginsDir 'keeppeek.exe')
    Assert-VerificationFails -CaseName 'unexpected-subdir-exe' -ExpectedPattern 'Unexpected executable directory' -Action {
        & $verifyScript `
            -InstallerPath $installerExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }
    Remove-Item -LiteralPath (Join-Path $pluginsDir 'keeppeek.exe')

    # Negative case 7: non-zero 7-Zip exit fails verification.
    $env:KEEPPEEK_FAKE_7Z_EXIT = '2'
    Assert-VerificationFails -CaseName 'sevenzip-failure' -ExpectedPattern '7-Zip failed to extract' -Action {
        & $verifyScript `
            -InstallerPath $installerExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }

    [Console]::WriteLine('Windows installer signature verification regression passed.')
} finally {
    $env:KEEPPEEK_FAKE_7Z_SRC = $previousSrc
    $env:KEEPPEEK_FAKE_7Z_EXIT = $previousExit
    $resolvedRoot = [System.IO.Path]::GetFullPath($testRoot)
    if ($resolvedRoot.StartsWith($temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase) -and
        [System.IO.Path]::GetFileName($resolvedRoot) -like 'keeppeek-sigcheck-test-*' -and
        [System.IO.Directory]::Exists($resolvedRoot)) {
        [System.IO.Directory]::Delete($resolvedRoot, $true)
    }
}
