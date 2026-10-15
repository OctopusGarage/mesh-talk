param(
    [Parameter(Mandatory = $true)]
    [string]$StagedBinary,
    [string]$Root = 'D:\ProgramData\MeshTalk\PostOffice',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$')]
    [string]$Name = 'post-office'
)

$ErrorActionPreference = 'Stop'
$taskName = 'MeshTalk-PostOffice'
$binary = Join-Path $Root 'mesh-talk-node.exe'
$data = Join-Path $Root 'data'
$passwordFile = Join-Path $data 'password'

if (-not (Test-Path -LiteralPath $StagedBinary -PathType Leaf)) {
    throw "Staged Windows binary is missing: $StagedBinary"
}

# SYSTEM runs the boot task; Administrators can maintain and back up the data.
# Restrict each parent before creating the password so it is never written
# beneath a directory that still grants inherited access to other users.
New-Item -ItemType Directory -Path $Root -Force | Out-Null
foreach ($directory in @($Root, $data)) {
    if ($directory -eq $data) {
        New-Item -ItemType Directory -Path $data -Force | Out-Null
    }
    & icacls.exe $directory '/grant:r' '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not grant relay access on $directory" }
    & icacls.exe $directory '/inheritance:r' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not remove inherited relay access on $directory" }
}
$allowedSids = @('S-1-5-18', 'S-1-5-32-544')
function Assert-RelayAcls {
    $paths = @($Root) + @(Get-ChildItem -LiteralPath $Root -Recurse -Force | ForEach-Object FullName)
    foreach ($path in $paths) {
        foreach ($entry in (Get-Acl -LiteralPath $path).Access) {
            $sid = $entry.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value
            if ($sid -notin $allowedSids) {
                throw "Unexpected access to relay path $path by $sid"
            }
        }
    }
}
Assert-RelayAcls

if (-not (Test-Path -LiteralPath $passwordFile)) {
    foreach ($existingState in @('identity.keystore', 'account.keystore', 'relay.log')) {
        if (Test-Path -LiteralPath (Join-Path $data $existingState)) {
            throw "Existing relay data needs its original password file; recover it before installing: $existingState"
        }
    }
    $bytes = New-Object byte[] 32
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $rng.GetBytes($bytes)
    } finally {
        $rng.Dispose()
    }
    $password = [Convert]::ToBase64String($bytes)
    [System.IO.File]::WriteAllText(
        $passwordFile,
        $password,
        (New-Object System.Text.UTF8Encoding($false))
    )
    [Array]::Clear($bytes, 0, $bytes.Length)
    Remove-Variable password
}
Assert-RelayAcls

$existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($existing) {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
}
Copy-Item -LiteralPath $StagedBinary -Destination $binary -Force

$arguments = '--keystore "{0}" --password-file "{1}" --name "{2}" --post-office --port 47475 --discovery-port 47474' -f (Join-Path $data 'identity.keystore'), $passwordFile, $Name
$action = New-ScheduledTaskAction -Execute $binary -Argument $arguments -WorkingDirectory $Root
$trigger = New-ScheduledTaskTrigger -AtStartup
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit (New-TimeSpan -Seconds 0) -StartWhenAvailable -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null

foreach ($rule in @(
    @{ Name = 'MeshTalk PostOffice TCP 47475'; Protocol = 'TCP'; Port = 47475 },
    @{ Name = 'MeshTalk PostOffice UDP 47474'; Protocol = 'UDP'; Port = 47474 }
)) {
    if (-not (Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -DisplayName $rule.Name -Direction Inbound -Action Allow -Protocol $rule.Protocol -LocalPort $rule.Port -RemoteAddress LocalSubnet -Profile Any | Out-Null
    }
}

Start-ScheduledTask -TaskName $taskName
$deadline = (Get-Date).AddSeconds(30)
do {
    $listener = Get-NetTCPConnection -State Listen -LocalPort 47475 -ErrorAction SilentlyContinue |
        Where-Object { $_.OwningProcess -in @(Get-Process -Name 'mesh-talk-node' -ErrorAction SilentlyContinue | ForEach-Object Id) } |
        Select-Object -First 1
    if ($listener -and (Get-ScheduledTask -TaskName $taskName).State -eq 'Running') {
        break
    }
    Start-Sleep -Milliseconds 500
} while ((Get-Date) -lt $deadline)
if (-not $listener -or (Get-ScheduledTask -TaskName $taskName).State -ne 'Running') {
    throw 'Post-office task did not start a TCP 47475 listener within 30 seconds'
}
Write-Output "Installed $binary"
Write-Output "Data $data"
Write-Output "Task $taskName"
Write-Output "SHA256 $((Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash)"
