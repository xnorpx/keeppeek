$ErrorActionPreference = 'Stop'
$temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$testRoot = [System.IO.Path]::Combine($temporaryRoot, 'keeppeek-acl-test-' + [guid]::NewGuid().ToString('N'))
$previousLoading = $PSModuleAutoLoadingPreference

try {
    [void][System.IO.Directory]::CreateDirectory([System.IO.Path]::Combine($testRoot, 'child'))
    $file = [System.IO.Path]::Combine($testRoot, 'child', 'evidence.txt')
    [System.IO.File]::WriteAllText($file, 'fixture')
    # Windows PowerShell must not load security cmdlets from an inherited PowerShell 7 module path.
    $PSModuleAutoLoadingPreference = 'None'
    $protect = [System.IO.Path]::Combine($PSScriptRoot, 'protect-test-directory.ps1')
    & $protect -Directory $testRoot -Recurse
    & $protect -Directory $testRoot -Recurse

    $owner = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
    $security = [System.IO.Directory]::GetAccessControl($testRoot)
    if (-not $security.AreAccessRulesProtected) {
        throw 'The fixture root still inherits permissions.'
    }
    $allowed = @($owner.Value, 'S-1-5-18', 'S-1-5-32-544')
    $rules = $security.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])
    if ($rules.Count -ne 3) { throw 'The fixture root has unexpected access rules.' }
    foreach ($rule in $rules) {
        if ($rule.IdentityReference.Value -notin $allowed -or $rule.IsInherited -or
            $rule.AccessControlType -ne [System.Security.AccessControl.AccessControlType]::Allow -or
            $rule.FileSystemRights -ne [System.Security.AccessControl.FileSystemRights]::FullControl -or
            $rule.InheritanceFlags -ne [System.Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit') {
            throw 'The fixture root has unsafe access rules.'
        }
    }
    $entries = @([System.IO.DirectoryInfo]::new($testRoot),
        [System.IO.DirectoryInfo]::new([System.IO.Path]::Combine($testRoot, 'child')),
        [System.IO.FileInfo]::new($file))
    foreach ($entry in $entries) {
        if ($entry.GetAccessControl().GetOwner([System.Security.Principal.SecurityIdentifier]) -ne $owner) {
            throw 'The fixture has an unexpected owner.'
        }
    }
    [Console]::WriteLine('Storage ACL regression passed without module autoloading.')
} finally {
    $PSModuleAutoLoadingPreference = $previousLoading
    $resolvedRoot = [System.IO.Path]::GetFullPath($testRoot)
    if (-not $resolvedRoot.StartsWith($temporaryRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        [System.IO.Path]::GetFileName($resolvedRoot) -notlike 'keeppeek-acl-test-*') {
        throw 'Refusing to remove a fixture outside the temporary directory.'
    }
    if ([System.IO.Directory]::Exists($resolvedRoot)) {
        [System.IO.Directory]::Delete($resolvedRoot, $true)
    }
}
