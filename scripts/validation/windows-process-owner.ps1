param([string]$Source)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
# The request/control pipe is private to this helper. Target stdin is a separate
# native pipe, so neither environment values nor control messages reach it.
$request = [Console]::ReadLine() | ConvertFrom-Json
Add-Type -Path $Source
$result = [WindowsCommandOwner]::Run([string]$request.app, [string[]]$request.args, [string[]]$request.environment, [string]$request.cwd)
$receipt = @{ schema = 1; nonce = $request.nonce; result = $result } | ConvertTo-Json -Compress
[IO.File]::WriteAllText([string]$request.receipt, $receipt, (New-Object Text.UTF8Encoding($false)))
