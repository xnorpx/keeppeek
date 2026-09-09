param(
    [Parameter(Mandatory = $true)]
    [string]$Directory,
    [switch]$Recurse
)

$ErrorActionPreference = 'Stop'
$owner = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$security = [System.Security.AccessControl.DirectorySecurity]::new()
$security.SetOwner($owner)
$security.SetAccessRuleProtection($true, $false)

foreach ($identity in @($owner.Value, 'S-1-5-18', 'S-1-5-32-544')) {
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        [System.Security.Principal.SecurityIdentifier]::new($identity),
        [System.Security.AccessControl.FileSystemRights]::FullControl,
        [System.Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit',
        [System.Security.AccessControl.PropagationFlags]::None,
        [System.Security.AccessControl.AccessControlType]::Allow
    )
    $security.AddAccessRule($rule)
}

[System.IO.Directory]::SetAccessControl($Directory, $security)

if ($Recurse) {
    $pending = [System.Collections.Generic.Queue[System.IO.DirectoryInfo]]::new()
    $pending.Enqueue([System.IO.DirectoryInfo]::new($Directory))
    $entryCount = 0
    while ($pending.Count -gt 0) {
        $parent = $pending.Dequeue()
        foreach ($entry in $parent.EnumerateFileSystemInfos()) {
            $entryCount += 1
            if ($entryCount -gt 128) {
                throw 'Test storage exceeds the ownership setup limit.'
            }
            if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'Test storage must not contain reparse points.'
            }
            $entrySecurity = $entry.GetAccessControl()
            $entrySecurity.SetOwner($owner)
            $entry.SetAccessControl($entrySecurity)
            if ($entry -is [System.IO.DirectoryInfo]) {
                $pending.Enqueue($entry)
            }
        }
    }
}