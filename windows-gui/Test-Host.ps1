# Opt-in interactive Windows check. Uses a fresh identity and loopback port 0;
# no client can authenticate, so native input always passes through.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$hostExe = Join-Path $projectRoot 'target\gui-host\release\windows-host.exe'
$fixtureDirectory = Join-Path $projectRoot ('target\gui-tests\native ' + [guid]::NewGuid().ToString('N'))
Push-Location $projectRoot
try {
    & $hostExe pair $fixtureDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Pairing fixture failed' }
    $monitor = @(& $hostExe monitors)[0]
    if ($monitor -notmatch '^left = (-?\d+)  top = (-?\d+)  width = (\d+)  height = (\d+)$') { throw 'No Windows display' }
    $config = Join-Path $fixtureDirectory 'host.local.toml'
    & $hostExe configure $config '127.0.0.1:0' $Matches[1] $Matches[2] $Matches[3] $Matches[4]
    if ($LASTEXITCODE -ne 0) { throw 'Configuration fixture failed' }
    foreach ($stopMode in @('eof', 'byte')) {
        $info = New-Object System.Diagnostics.ProcessStartInfo
        $info.FileName = $hostExe
        $info.Arguments = '--managed "' + $config + '"'
        $info.WorkingDirectory = $projectRoot
        $info.UseShellExecute = $false
        $info.CreateNoWindow = $true
        $info.RedirectStandardInput = $true
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $process = New-Object System.Diagnostics.Process
        $process.StartInfo = $info
        try {
            $process.Start() | Out-Null
            $errors = $process.StandardError.ReadToEndAsync()
            $line = $process.StandardOutput.ReadLineAsync()
            if (!$line.Wait(8000) -or $line.Result -notlike 'Host listening on*') {
                if ($process.HasExited) { throw ('Host did not reach listening state: ' + $errors.Result) }
                throw 'Host did not reach listening state'
            }
            if ($stopMode -eq 'byte') { $process.StandardInput.WriteLine('stop'); $process.StandardInput.Flush() }
            else { $process.StandardInput.Close() }
            if (!$process.WaitForExit(8000)) { throw 'Managed shutdown timed out' }
            if ($process.ExitCode -ne 0) { throw ('Host failed: ' + $errors.Result) }
            Write-Output "PASS: native host startup and managed $stopMode shutdown"
        } finally {
            if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
            $process.Dispose()
        }
    }
} finally { Pop-Location }
