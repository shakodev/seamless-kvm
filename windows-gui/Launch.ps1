param([switch]$BuildOnly)
$ErrorActionPreference = 'Stop'
try {
    $projectRoot = Split-Path $PSScriptRoot -Parent
    $outputDirectory = Join-Path $projectRoot 'target\gui'
    $executable = Join-Path $outputDirectory 'SeamlessKVM.exe'
    $sources = @((Join-Path $PSScriptRoot 'App.cs'), (Join-Path $PSScriptRoot 'MainWindow.xaml'), $PSCommandPath)
    $newest = ($sources | Get-Item | Sort-Object LastWriteTime -Descending | Select-Object -First 1).LastWriteTime
    if (!(Test-Path $executable) -or (Get-Item $executable).LastWriteTime -lt $newest) {
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
        $framework = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319'
        $arguments = @('/nologo', '/target:winexe', '/optimize+', "/out:$executable",
            '/reference:System.dll', '/reference:System.Core.dll', '/reference:System.Xaml.dll',
            "/reference:$framework\WPF\WindowsBase.dll",
            "/reference:$framework\WPF\PresentationCore.dll",
            "/reference:$framework\WPF\PresentationFramework.dll",
            "/resource:$PSScriptRoot\MainWindow.xaml,MainWindow.xaml",
            (Join-Path $PSScriptRoot 'App.cs'))
        $result = & (Join-Path $framework 'csc.exe') @arguments 2>&1
        if ($LASTEXITCODE -ne 0) { throw ($result -join [Environment]::NewLine) }
    }
    if (!$BuildOnly) { Start-Process -FilePath $executable -WorkingDirectory $projectRoot -WindowStyle Hidden }
} catch {
    if ($BuildOnly) { throw }
    Add-Type -AssemblyName PresentationFramework
    [System.Windows.MessageBox]::Show($_.Exception.Message, 'SeamlessKVM could not open', 'OK', 'Error') | Out-Null
    exit 1
}
