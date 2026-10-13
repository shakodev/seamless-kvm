$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$outputDirectory = Join-Path $projectRoot 'target\gui-tests'
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
$framework = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319'
$compiler = Join-Path $framework 'csc.exe'
$fixture = Join-Path $outputDirectory 'Fixture.exe'
& $compiler /nologo /target:exe "/out:$fixture" (Join-Path $PSScriptRoot 'tests\Fixture.cs')
if ($LASTEXITCODE -ne 0) { throw 'Fixture compilation failed' }
$testExe = Join-Path $outputDirectory 'SmokeTests.exe'
$arguments = @('/nologo', '/target:exe', '/main:SeamlessKVM.SmokeTests', "/out:$testExe",
    '/reference:System.dll', '/reference:System.Core.dll', '/reference:System.Xaml.dll',
    "/reference:$framework\WPF\WindowsBase.dll", "/reference:$framework\WPF\PresentationCore.dll",
    "/reference:$framework\WPF\PresentationFramework.dll", "/resource:$PSScriptRoot\MainWindow.xaml,MainWindow.xaml",
    (Join-Path $PSScriptRoot 'App.cs'), (Join-Path $PSScriptRoot 'tests\SmokeTests.cs'))
& $compiler @arguments
if ($LASTEXITCODE -ne 0) { throw 'Test compilation failed' }
$fixtureRoot = Join-Path $outputDirectory ('fixture workspace ' + [guid]::NewGuid().ToString('N'))
& $testExe $fixtureRoot $fixture
if ($LASTEXITCODE -ne 0) { throw 'GUI smoke tests failed' }
