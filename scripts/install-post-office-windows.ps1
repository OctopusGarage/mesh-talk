param(
    [Parameter(Mandatory = $true)]
    [string]$StagedBinary,
    [string]$Root = 'D:\ProgramData\MeshTalk\PostOffice',
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$')]
    [string]$Name = 'post-office',
    [switch]$Bridge,
    [string]$BridgePeer,
    [ValidatePattern('^[0-9A-Fa-f]{32}$')]
    [string]$BridgePeerId,
    [ValidateRange(1, 65535)]
    [int]$BridgeProxyPort = 47476,
    [ValidateRange(1, 65535)]
    [int]$DiscoveryPort = 47478,
    [ValidateRange(1, 65535)]
    [int]$LanAnnouncePort = 47474,
    [string[]]$FederationPeer = @(),
    [string]$FederationConfig,
    [string]$RelayNetwork,
    [string]$RelayMembership,
    [string[]]$RelaySeed = @()
)

$ErrorActionPreference = 'Stop'
if ($DiscoveryPort -ne 47478 -or $LanAnnouncePort -ne 47474) {
    throw 'Managed post offices require UDP 47478 and LAN UDP 47474'
}
$taskName = 'MeshTalk-PostOffice'
$Root = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Root)
$Root = [System.IO.Path]::GetFullPath($Root)
$binary = Join-Path $Root 'mesh-talk-node.exe'
$data = Join-Path $Root 'data'
$passwordFile = Join-Path $data 'password'

if ([bool]$BridgePeer -ne [bool]$BridgePeerId) {
    throw 'BridgePeer and BridgePeerId must be supplied together'
}
if ($BridgePeer -and $BridgeProxyPort -eq 47479) {
    throw 'The bridge proxy port must differ from the local relay port 47479'
}
foreach ($seed in $FederationPeer) {
    if ($seed -notmatch '^[0-9A-Fa-f]{32}@.+:[0-9]+$') {
        throw "Invalid federation peer: $seed"
    }
}

if (-not (Test-Path -LiteralPath $StagedBinary -PathType Leaf)) {
    throw "Staged Windows binary is missing: $StagedBinary"
}

if ($FederationConfig -and $FederationPeer.Count -gt 0) {
    throw 'FederationConfig conflicts with FederationPeer'
}
if ($FederationConfig) {
    if (-not (Test-Path -LiteralPath $FederationConfig -PathType Leaf)) {
        throw "Federation config must be an existing public manifest: $FederationConfig"
    }
    $FederationConfig = (Resolve-Path -LiteralPath $FederationConfig).ProviderPath
    # The staged node owns schema, endpoint, and bound validation. No installation
    # state has been changed if the public manifest or the binary is incompatible.
    $StagedBinary = (Resolve-Path -LiteralPath $StagedBinary).ProviderPath
    & $StagedBinary federation --file $FederationConfig list | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Staged node rejected the federation config' }
}

if ($RelayNetwork -or $RelayMembership -or $RelaySeed.Count -gt 0) {
    if (-not $RelayNetwork -or -not $RelayMembership -or $FederationConfig -or $FederationPeer.Count -gt 0 -or $BridgePeer) {
        throw 'Network mode needs policy and membership and conflicts with federation pins or a bridge gateway'
    }
    foreach ($seed in $RelaySeed) {
        if ($seed -notmatch '^[0-9A-Fa-f]{32}@.+:[0-9]+$') { throw "Invalid relay seed: $seed" }
    }
    $RelayNetwork = (Resolve-Path -LiteralPath $RelayNetwork).ProviderPath
    $RelayMembership = (Resolve-Path -LiteralPath $RelayMembership).ProviderPath
    $StagedBinary = (Resolve-Path -LiteralPath $StagedBinary).ProviderPath
    & $StagedBinary network inspect --policy $RelayNetwork --membership $RelayMembership | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Staged node rejected the network policy or membership' }
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

if ($FederationConfig) {
    # Keep the public manifest after staging cleanup without touching private state.
    $persistentConfig = Join-Path $data 'federation.conf'
    if (-not [string]::Equals($FederationConfig, $persistentConfig, [System.StringComparison]::OrdinalIgnoreCase)) {
        Copy-Item -LiteralPath $FederationConfig -Destination "$persistentConfig.next" -Force
        Move-Item -LiteralPath "$persistentConfig.next" -Destination $persistentConfig -Force
    }
    $FederationConfig = $persistentConfig
}

if ($RelayNetwork) {
    foreach ($entry in @(@($RelayNetwork, 'network.policy'), @($RelayMembership, 'relay.membership'))) {
        $persistentFile = Join-Path $data $entry[1]
        if (-not [string]::Equals($entry[0], $persistentFile, [System.StringComparison]::OrdinalIgnoreCase)) {
            Copy-Item -LiteralPath $entry[0] -Destination "$persistentFile.next" -Force
            Move-Item -LiteralPath "$persistentFile.next" -Destination $persistentFile -Force
        }
    }
    $RelayNetwork = Join-Path $data 'network.policy'
    $RelayMembership = Join-Path $data 'relay.membership'
    Assert-RelayAcls
}

$existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($existing) {
    Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
}
Copy-Item -LiteralPath $StagedBinary -Destination $binary -Force

$arguments = '--keystore "{0}" --password-file "{1}" --name "{2}" --post-office --port 47479 --discovery-port {3} --lan-announce-port {4}' -f (Join-Path $data 'identity.keystore'), $passwordFile, $Name, $DiscoveryPort, $LanAnnouncePort
if ($BridgePeer) {
    $arguments += ' --bridge --bridge-peer "{0}" --bridge-peer-id "{1}" --bridge-proxy-port {2}' -f $BridgePeer, $BridgePeerId.ToLowerInvariant(), $BridgeProxyPort
} elseif ($Bridge -or $FederationConfig -or $FederationPeer.Count -gt 0) {
    $arguments += ' --bridge'
}
if ($FederationConfig) {
    $arguments += ' --federation-config "{0}"' -f $FederationConfig
}
foreach ($seed in $FederationPeer) {
    $arguments += ' --federation-peer "{0}"' -f $seed
}
if ($RelayNetwork) {
    $arguments += ' --relay-network "{0}" --relay-membership "{1}"' -f $RelayNetwork, $RelayMembership
}
foreach ($seed in $RelaySeed) {
    $arguments += ' --relay-seed "{0}"' -f $seed
}
$action = New-ScheduledTaskAction -Execute $binary -Argument $arguments -WorkingDirectory $Root
$trigger = New-ScheduledTaskTrigger -AtStartup
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit (New-TimeSpan -Seconds 0) -StartWhenAvailable -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null

$firewallRules = @(
    @{ Name = 'MeshTalk PostOffice TCP 47479'; Protocol = 'TCP'; Port = 47479 },
    @{ Name = "MeshTalk PostOffice UDP $DiscoveryPort"; Protocol = 'UDP'; Port = $DiscoveryPort }
)
if ($BridgePeer) {
    $firewallRules += @{ Name = "MeshTalk PostOffice Bridge TCP $BridgeProxyPort"; Protocol = 'TCP'; Port = $BridgeProxyPort }
}
foreach ($rule in $firewallRules) {
    if (-not (Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -DisplayName $rule.Name -Direction Inbound -Action Allow -Protocol $rule.Protocol -LocalPort $rule.Port -RemoteAddress LocalSubnet -Profile Any | Out-Null
    }
}

Start-ScheduledTask -TaskName $taskName
$deadline = (Get-Date).AddSeconds(30)
do {
    $listener = Get-NetTCPConnection -State Listen -LocalPort 47479 -ErrorAction SilentlyContinue |
        Where-Object { $_.OwningProcess -in @(Get-Process -Name 'mesh-talk-node' -ErrorAction SilentlyContinue | ForEach-Object Id) } |
        Select-Object -First 1
    if ($listener -and (Get-ScheduledTask -TaskName $taskName).State -eq 'Running') {
        break
    }
    Start-Sleep -Milliseconds 500
} while ((Get-Date) -lt $deadline)
if (-not $listener -or (Get-ScheduledTask -TaskName $taskName).State -ne 'Running') {
    throw 'Post-office task did not start a TCP 47479 listener within 30 seconds'
}
if ($BridgePeer) {
    $proxyDeadline = (Get-Date).AddSeconds(45)
    do {
        $proxy = Get-NetTCPConnection -State Listen -LocalPort $BridgeProxyPort -ErrorAction SilentlyContinue |
            Where-Object { $_.OwningProcess -eq $listener.OwningProcess } |
            Select-Object -First 1
        if ($proxy) { break }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $proxyDeadline)
    if (-not $proxy) {
        throw "Bridge gateway did not bind TCP $BridgeProxyPort within 45 seconds"
    }
}
Write-Output "Installed $binary"
Write-Output "Data $data"
Write-Output "Task $taskName"
Write-Output "SHA256 $((Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash)"
