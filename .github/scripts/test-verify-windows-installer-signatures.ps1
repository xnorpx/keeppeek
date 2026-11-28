$ErrorActionPreference = 'Stop'

function Get-SignedFixtureSet {
    $gitRoot = Join-Path $env:ProgramFiles 'Git'
    $candidates = @(
        (Join-Path $gitRoot 'bin\sh.exe'),
        (Join-Path $gitRoot 'bin\bash.exe'),
        (Join-Path $gitRoot 'git-bash.exe'),
        (Join-Path $gitRoot 'git-cmd.exe'),
        (Join-Path $gitRoot 'bin\git.exe'),
        (Join-Path $gitRoot 'cmd\git.exe'),
        (Join-Path $gitRoot 'cmd\git-gui.exe'),
        (Join-Path $gitRoot 'cmd\gitk.exe'),
        (Join-Path $env:ProgramFiles 'PowerShell\7\pwsh.exe'),
        (Join-Path $env:ProgramFiles 'GitHub CLI\gh.exe')
    )

    $byThumbprint = @{}
    foreach ($candidate in $candidates) {
        if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            continue
        }
        $signature = Get-AuthenticodeSignature -LiteralPath $candidate
        if ($signature.Status -ne 'Valid' -or
            $signature.SignatureType -ne 'Authenticode' -or
            $null -eq $signature.SignerCertificate -or
            $null -eq $signature.TimestamperCertificate) {
            continue
        }
        $thumbprint = $signature.SignerCertificate.Thumbprint
        $sha = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash
        if (-not $byThumbprint.ContainsKey($thumbprint)) {
            $byThumbprint[$thumbprint] = [System.Collections.Generic.List[hashtable]]::new()
        }
        $duplicateSha = $false
        foreach ($existing in $byThumbprint[$thumbprint]) {
            if ($existing.Sha -eq $sha) { $duplicateSha = $true; break }
        }
        if (-not $duplicateSha) {
            $byThumbprint[$thumbprint].Add(@{ Path = $candidate; Sha = $sha })
        }
    }

    $pairThumbprint = $null
    foreach ($tp in ($byThumbprint.Keys | Sort-Object)) {
        if ($byThumbprint[$tp].Count -ge 2) {
            $pairThumbprint = $tp
            break
        }
    }
    if ($null -eq $pairThumbprint) {
        throw 'Test requires two distinct embedded-Authenticode executables from one signer.'
    }

    $differentSigner = $null
    foreach ($tp in ($byThumbprint.Keys | Sort-Object)) {
        if ($tp -ne $pairThumbprint -and $byThumbprint[$tp].Count -ge 1) {
            $differentSigner = $byThumbprint[$tp][0].Path
            break
        }
    }

    $catalogExe = Join-Path $env:SystemRoot 'System32\sc.exe'
    $catalogSig = Get-AuthenticodeSignature -LiteralPath $catalogExe
    if ($catalogSig.Status -ne 'Valid' -or $catalogSig.SignatureType -ne 'Catalog') {
        $catalogExe = $null
    }

    return @{
        Primary         = $byThumbprint[$pairThumbprint][0].Path
        SameSigner      = $byThumbprint[$pairThumbprint][1].Path
        DifferentSigner = $differentSigner
        CatalogOnly     = $catalogExe
    }
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
    } finally {
        $global:LASTEXITCODE = 0
    }
    if (-not $failed) {
        throw "Case '$CaseName' succeeded unexpectedly."
    }
}

function Remove-TestRootSafely {
    param(
        [Parameter(Mandatory = $true)]
        [string]$TargetRoot,
        [Parameter(Mandatory = $true)]
        [string]$AllowedParent
    )

    $resolvedRoot = [System.IO.Path]::GetFullPath($TargetRoot)
    if (-not $resolvedRoot.StartsWith($AllowedParent, [System.StringComparison]::OrdinalIgnoreCase) -or
        [System.IO.Path]::GetFileName($resolvedRoot) -notlike 'keeppeek-sigcheck-test-*' -or
        -not [System.IO.Directory]::Exists($resolvedRoot)) {
        return
    }

    # Unlink reparse points without traversing targets because PowerShell 5.1 Remove-Item follows junctions.
    $pending = [System.Collections.Generic.Queue[System.IO.DirectoryInfo]]::new()
    $pending.Enqueue([System.IO.DirectoryInfo]::new($resolvedRoot))
    $entryCount = 0
    while ($pending.Count -gt 0 -and $entryCount -lt 1024) {
        $directory = $pending.Dequeue()
        foreach ($entry in $directory.EnumerateFileSystemInfos()) {
            $entryCount += 1
            if ($entryCount -ge 1024) { break }
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
    if ($pending.Count -gt 0 -or $entryCount -ge 1024) {
        return
    }
    [System.IO.Directory]::Delete($resolvedRoot, $true)
}

function New-FakeSevenZipScript {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $lines = @(
        '@echo off',
        'setlocal enabledelayedexpansion',
        'set "OUT="',
        ':parse',
        'if "%~1"=="" goto done_parse',
        'set "ARG=%~1"',
        'if "!ARG:~0,2!"=="-o" set "OUT=!ARG:~2!"',
        'shift',
        'goto parse',
        ':done_parse',
        'if not "%KEEPPEEK_FAKE_7Z_MSG%"=="" echo %KEEPPEEK_FAKE_7Z_MSG%',
        'if not "%KEEPPEEK_FAKE_7Z_WARN%"=="" echo %KEEPPEEK_FAKE_7Z_WARN%',
        'if "%KEEPPEEK_FAKE_7Z_EMPTY%"=="1" goto finish',
        'xcopy /E /I /Y /Q "%KEEPPEEK_FAKE_7Z_SRC%\*" "%OUT%\" >nul',
        'if errorlevel 1 exit /b %ERRORLEVEL%',
        ':finish',
        'if not "%KEEPPEEK_FAKE_7Z_EXIT%"=="" exit /b %KEEPPEEK_FAKE_7Z_EXIT%',
        'exit /b 0'
    )
    [System.IO.File]::WriteAllText($Path, ($lines -join "`r`n"))
}

$verifyScript = [System.IO.Path]::Combine(
    $PSScriptRoot,
    'verify-windows-installer-signatures.ps1'
)
$fixtures = Get-SignedFixtureSet
$temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$testRoot = [System.IO.Path]::Combine(
    $temporaryRoot,
    'keeppeek-sigcheck-test-' + [guid]::NewGuid().ToString('N')
)
$savedEnv = @{
    KEEPPEEK_FAKE_7Z_SRC   = $env:KEEPPEEK_FAKE_7Z_SRC
    KEEPPEEK_FAKE_7Z_EXIT  = $env:KEEPPEEK_FAKE_7Z_EXIT
    KEEPPEEK_FAKE_7Z_MSG   = $env:KEEPPEEK_FAKE_7Z_MSG
    KEEPPEEK_FAKE_7Z_WARN  = $env:KEEPPEEK_FAKE_7Z_WARN
    KEEPPEEK_FAKE_7Z_EMPTY = $env:KEEPPEEK_FAKE_7Z_EMPTY
}

try {
    $signedDir = Join-Path $testRoot 'signed'
    $payloadDir = Join-Path $testRoot 'payload'
    $instSubDir = Join-Path $testRoot 'payload-instdir\$INSTDIR'
    $pluginsDir = Join-Path $payloadDir '$PLUGINSDIR'
    $junctionTarget = Join-Path $testRoot 'junction-target'
    foreach ($dir in $signedDir, $payloadDir, $instSubDir, $pluginsDir, $junctionTarget) {
        [void][System.IO.Directory]::CreateDirectory($dir)
    }

    $installerExe = Join-Path $testRoot 'keeppeek-master-windows-x86_64-installer.exe'
    $signedUninstaller = Join-Path $testRoot 'uninstall.exe'
    $unsignedExe = Join-Path $testRoot 'unsigned.exe'
    Copy-Item -LiteralPath $fixtures.Primary -Destination $installerExe
    Copy-Item -LiteralPath $fixtures.SameSigner -Destination $signedUninstaller
    [System.IO.File]::WriteAllText($unsignedExe, 'not-a-signed-pe')

    $inputMap = @{
        'keeppeek.exe'            = $fixtures.Primary
        'keeppeek-service.exe'    = $fixtures.Primary
        'keeppeek-supervisor.exe' = $fixtures.SameSigner
        'keeppeek-desktop.exe'    = $fixtures.Primary
    }
    foreach ($name in $inputMap.Keys) {
        Copy-Item -LiteralPath $inputMap[$name] -Destination (Join-Path $signedDir $name)
        Copy-Item -LiteralPath $inputMap[$name] -Destination (Join-Path $payloadDir $name)
        Copy-Item -LiteralPath $inputMap[$name] -Destination (Join-Path $instSubDir $name)
    }
    Copy-Item -LiteralPath $signedUninstaller -Destination (Join-Path $payloadDir 'uninstall.exe')
    Copy-Item -LiteralPath $signedUninstaller -Destination (Join-Path $instSubDir 'uninstall.exe')
    Copy-Item -LiteralPath $fixtures.Primary -Destination (Join-Path $payloadDir 'keeppeek_1.exe')
    [System.IO.File]::WriteAllText((Join-Path $pluginsDir 'nsExec.dll'), 'plugin-stub')

    $fakeSevenZip = Join-Path $testRoot 'fake-7z.cmd'
    New-FakeSevenZipScript -Path $fakeSevenZip

    $runVerify = {
        param([string]$TargetInstaller = $installerExe)
        & $verifyScript `
            -InstallerPath $TargetInstaller `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeSevenZip
    }

    $env:KEEPPEEK_FAKE_7Z_SRC = $payloadDir
    $env:KEEPPEEK_FAKE_7Z_EXIT = ''
    $env:KEEPPEEK_FAKE_7Z_MSG = ''
    $env:KEEPPEEK_FAKE_7Z_WARN = ''
    $env:KEEPPEEK_FAKE_7Z_EMPTY = ''

    # Verify a signed installer with distinct signed binaries, a duplicate copy, and a plugin DLL.
    & $runVerify

    # Verify an extracted layout rooted under the $INSTDIR subdirectory.
    $env:KEEPPEEK_FAKE_7Z_SRC = Join-Path $testRoot 'payload-instdir'
    & $runVerify
    $env:KEEPPEEK_FAKE_7Z_SRC = $payloadDir

    # Verify that the script allows 7-Zip exit code 1 for both trailing-data warning messages.
    foreach ($tailMsg in @(
        'There are data after the end of archive',
        'There are some data after the end of the payload data'
    )) {
        $env:KEEPPEEK_FAKE_7Z_EXIT = '1'
        $env:KEEPPEEK_FAKE_7Z_MSG = $tailMsg
        $env:KEEPPEEK_FAKE_7Z_WARN = 'Warnings: 1'
        & $runVerify
    }
    $env:KEEPPEEK_FAKE_7Z_EXIT = ''
    $env:KEEPPEEK_FAKE_7Z_MSG = ''
    $env:KEEPPEEK_FAKE_7Z_WARN = ''

    # Reject an unsigned outer installer before extraction starts.
    Assert-VerificationFails -CaseName 'unsigned-installer' `
        -ExpectedPattern 'invalid Authenticode signature status' -Action {
            & $runVerify -TargetInstaller $unsignedExe
        }

    # Reject a Windows Catalog-only executable that lacks an embedded Authenticode signature.
    if ($null -ne $fixtures.CatalogOnly) {
        Assert-VerificationFails -CaseName 'catalog-only-installer' `
            -ExpectedPattern 'embedded Authenticode signature' -Action {
                & $runVerify -TargetInstaller $fixtures.CatalogOnly
            }
    }

    # Reject an embedded executable signed by a different certificate thumbprint.
    if ($null -ne $fixtures.DifferentSigner) {
        Copy-Item -LiteralPath $fixtures.DifferentSigner `
            -Destination (Join-Path $payloadDir 'keeppeek.exe') -Force
        Assert-VerificationFails -CaseName 'different-signer-thumbprint' `
            -ExpectedPattern 'signer thumbprint mismatch' -Action { & $runVerify }
        Copy-Item -LiteralPath $fixtures.Primary `
            -Destination (Join-Path $payloadDir 'keeppeek.exe') -Force
    }

    # Reject an unsigned embedded uninstaller.
    Copy-Item -LiteralPath $unsignedExe -Destination (Join-Path $payloadDir 'uninstall.exe') -Force
    Assert-VerificationFails -CaseName 'unsigned-embedded-uninstaller' `
        -ExpectedPattern 'invalid Authenticode signature status' -Action { & $runVerify }
    Copy-Item -LiteralPath $signedUninstaller -Destination (Join-Path $payloadDir 'uninstall.exe') -Force

    # Reject a signed embedded binary whose SHA-256 does not match its expected input.
    Copy-Item -LiteralPath $fixtures.SameSigner -Destination (Join-Path $payloadDir 'keeppeek.exe') -Force
    Assert-VerificationFails -CaseName 'hash-mismatch' `
        -ExpectedPattern 'SHA-256 mismatch' -Action { & $runVerify }
    Copy-Item -LiteralPath $fixtures.Primary -Destination (Join-Path $payloadDir 'keeppeek.exe') -Force

    # Reject a tampered duplicate section copy even when the primary copy matches.
    Copy-Item -LiteralPath $fixtures.SameSigner -Destination (Join-Path $payloadDir 'keeppeek_1.exe') -Force
    Assert-VerificationFails -CaseName 'duplicate-copy-hash-mismatch' `
        -ExpectedPattern 'SHA-256 mismatch' -Action { & $runVerify }
    Copy-Item -LiteralPath $fixtures.Primary -Destination (Join-Path $payloadDir 'keeppeek_1.exe') -Force

    # Reject an installer that omits a required signed binary.
    Remove-Item -LiteralPath (Join-Path $payloadDir 'keeppeek-supervisor.exe')
    Assert-VerificationFails -CaseName 'missing-supervisor' `
        -ExpectedPattern 'missing required signed executable: keeppeek-supervisor\.exe' `
        -Action { & $runVerify }
    Copy-Item -LiteralPath $fixtures.SameSigner `
        -Destination (Join-Path $payloadDir 'keeppeek-supervisor.exe') -Force

    # Reject an unexpected extra executable inside the installer payload.
    Copy-Item -LiteralPath $fixtures.Primary -Destination (Join-Path $payloadDir 'extra.exe')
    Assert-VerificationFails -CaseName 'unexpected-exe' `
        -ExpectedPattern 'Unexpected executable extracted from installer' -Action { & $runVerify }
    Remove-Item -LiteralPath (Join-Path $payloadDir 'extra.exe')

    # Reject an executable placed inside an unexpected subdirectory.
    Copy-Item -LiteralPath $fixtures.Primary -Destination (Join-Path $pluginsDir 'keeppeek.exe')
    Assert-VerificationFails -CaseName 'unexpected-subdir-exe' `
        -ExpectedPattern 'Unexpected executable directory' -Action { & $runVerify }
    Remove-Item -LiteralPath (Join-Path $pluginsDir 'keeppeek.exe')

    # Reject a missing signed input binary before extraction starts.
    Remove-Item -LiteralPath (Join-Path $signedDir 'keeppeek-supervisor.exe')
    Assert-VerificationFails -CaseName 'missing-signed-input' `
        -ExpectedPattern 'Required signed binary does not exist' -Action { & $runVerify }
    Copy-Item -LiteralPath $fixtures.SameSigner `
        -Destination (Join-Path $signedDir 'keeppeek-supervisor.exe')

    # Reject an empty extraction that yields zero executables.
    $env:KEEPPEEK_FAKE_7Z_EMPTY = '1'
    Assert-VerificationFails -CaseName 'empty-extraction' `
        -ExpectedPattern 'No executables were extracted' -Action { & $runVerify }
    $env:KEEPPEEK_FAKE_7Z_EMPTY = ''

    # Reject 7-Zip exit code 1 when the warning is not the expected trailing-tail warning.
    $env:KEEPPEEK_FAKE_7Z_EXIT = '1'
    $env:KEEPPEEK_FAKE_7Z_MSG = 'Unexpected extraction warning'
    Assert-VerificationFails -CaseName 'sevenzip-unexpected-warning' `
        -ExpectedPattern '7-Zip failed to extract' -Action { & $runVerify }

    # Reject a non-zero 7-Zip fatal exit code.
    $env:KEEPPEEK_FAKE_7Z_EXIT = '2'
    $env:KEEPPEEK_FAKE_7Z_MSG = 'Fatal extraction error'
    Assert-VerificationFails -CaseName 'sevenzip-failure' `
        -ExpectedPattern '7-Zip failed to extract' -Action { & $runVerify }
    $env:KEEPPEEK_FAKE_7Z_EXIT = ''
    $env:KEEPPEEK_FAKE_7Z_MSG = ''

    # Verify that reparse points fail closed and cleanup never deletes files in the junction target.
    $canaryFile = Join-Path $junctionTarget 'canary.txt'
    [System.IO.File]::WriteAllText($canaryFile, 'keep')
    $fakeJunctionCmd = Join-Path $testRoot 'fake-7z-junction.cmd'
    $junctionLines = @(
        '@echo off',
        'setlocal enabledelayedexpansion',
        'set "OUT="',
        ':parse',
        'if "%~1"=="" goto done_parse',
        'set "ARG=%~1"',
        'if "!ARG:~0,2!"=="-o" set "OUT=!ARG:~2!"',
        'shift',
        'goto parse',
        ':done_parse',
        "mklink /J `"!OUT!\linked`" `"$junctionTarget`" >nul",
        'exit /b 0'
    )
    [System.IO.File]::WriteAllText($fakeJunctionCmd, ($junctionLines -join "`r`n"))
    Assert-VerificationFails -CaseName 'reparse-point' -ExpectedPattern 'reparse point' -Action {
        & $verifyScript `
            -InstallerPath $installerExe `
            -SignedBinariesDir $signedDir `
            -SignedUninstallerPath $signedUninstaller `
            -SevenZipPath $fakeJunctionCmd
    }
    if (-not (Test-Path -LiteralPath $canaryFile -PathType Leaf) -or
        [System.IO.File]::ReadAllText($canaryFile) -ne 'keep') {
        throw 'Cleanup traversed the directory junction and damaged the canary file.'
    }

    [Console]::WriteLine('Windows installer signature verification regression passed.')
    exit 0
} finally {
    foreach ($key in $savedEnv.Keys) {
        [System.Environment]::SetEnvironmentVariable($key, $savedEnv[$key], 'Process')
    }
    try {
        Remove-TestRootSafely -TargetRoot $testRoot -AllowedParent $temporaryRoot
    } catch {
        # Keep cleanup best-effort so cleanup never hides a test failure.
    }
}
