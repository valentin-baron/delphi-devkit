<#
.SYNOPSIS
    Compiles a Delphi project using MSBuild with configurable parameters.

.DESCRIPTION
    This script provides a flexible way to compile Delphi projects with customizable
    MSBuild path, build arguments, and display messages.

.PARAMETER ProjectPath
    The full path to the Delphi project file (.dproj).

.PARAMETER RSVarsPath
    The path to the Delphi RSVars.bat file to set up the environment.

.PARAMETER MSBuildPath
    The path to MSBuild.exe.

.PARAMETER FileName
    Display name for the file in output messages.

.PARAMETER ActionDescription
    Description of the action being performed.

.PARAMETER PathDescription
    Description of the path context.

.PARAMETER BuildArguments
    String containing space-separated MSBuild arguments.

.PARAMETER CompilerName
    Display name of the compiler being used.

.EXAMPLE
    .\compile.ps1 -ProjectPath "C:\MyProject\MyApp.dproj" -RSVarsPath "C:\Program Files\Embarcadero\Studio\21.0\bin\rsvars.bat" -MSBuildPath "C:\Windows\Microsoft.NET\Framework\v4.0.30319\MSBuild.exe" -FileName "MyApp.dproj" -ActionDescription "compile / recreate" -PathDescription "MyProject folder" -BuildArguments "/t:Clean,Build /verbosity:minimal /p:Configuration=Debug" -CompilerName "Delphi 12"

.EXAMPLE
    .\compile.ps1 -ProjectPath "C:\MyProject\MyApp.dproj" -RSVarsPath "C:\Program Files\Embarcadero\Studio\21.0\bin\rsvars.bat" -MSBuildPath "C:\Windows\Microsoft.NET\Framework\v4.0.30319\MSBuild.exe" -FileName "MyApp.dproj" -ActionDescription "Building release version" -PathDescription "Production build" -BuildArguments "/t:Clean,Build /p:Configuration=Release" -CompilerName "Delphi 12"

.EXAMPLE
    # For Delphi 2007 with custom arguments
    .\compile.ps1 -ProjectPath "C:\MyProject\MyApp.dproj" -RSVarsPath "C:\Program Files\CodeGear\RAD Studio\5.0\bin\rsvars.bat" -MSBuildPath "C:\Windows\Microsoft.NET\Framework\v3.5\MSBuild.exe" -FileName "Legacy App" -ActionDescription "Building legacy project" -PathDescription "Delphi 2007 project" -BuildArguments "/t:Clean,Build /verbosity:minimal /p:DCC_DebugInformation=True /p:Configuration=Debug /p:RuntimeIdentifiers=3.5" -CompilerName "Delphi 2007"
#>

param(
    [Parameter(Mandatory=$true)]
    [string]$ProjectPath,

    [Parameter(Mandatory=$true)]
    [string]$RSVarsPath,

    [Parameter(Mandatory=$true)]
    [string]$MSBuildPath,

    [Parameter(Mandatory=$true)]
    [string]$FileName,

    [Parameter(Mandatory=$true)]
    [string]$ActionDescription,

    [Parameter(Mandatory=$true)]
    [string]$PathDescription,

    [Parameter(Mandatory=$true)]
    [string]$BuildArguments,

    [Parameter(Mandatory=$true)]
    [string]$CompilerName
)

Write-Host "++++++++++++++++++++++++++++++++START++++++++++++++++++++++++++++++++" -ForegroundColor Green
Write-Host -NoNewline "+++" -ForegroundColor Green; Write-Host " Delphi Utils - $FileName" -ForegroundColor Yellow
Write-Host -NoNewline "+++" -ForegroundColor Green; Write-Host " Action: $ActionDescription" -ForegroundColor Yellow
Write-Host -NoNewline "+++" -ForegroundColor Green; Write-Host " Path: $PathDescription" -ForegroundColor Yellow
Write-Host -NoNewline "+++" -ForegroundColor Green; Write-Host " Compiler: $CompilerName" -ForegroundColor Yellow
Write-Host "++++++++++++++++++++++++++++++++START++++++++++++++++++++++++++++++++" -ForegroundColor Green

# Call RSVars to set up Delphi environment and capture environment variables
Write-Host "Setting up Delphi environment..." -ForegroundColor Cyan

# Run RSVars and capture the environment after
$tempBatch = [System.IO.Path]::GetTempFileName() + ".bat"
try {
    # Create a batch file that calls RSVars and then outputs all environment variables
    @"
@echo off
call "$RSVarsPath"
set
"@ | Out-File -FilePath $tempBatch -Encoding ASCII

    # Execute the batch file and capture output
    $envOutput = & cmd.exe /c $tempBatch

    # Parse the environment variables from the output
    foreach ($line in $envOutput) {
        if ($line -match '^([^=]+)=(.*)$') {
            $varName = $matches[1]
            $varValue = $matches[2]

            # Skip system variables that shouldn't be changed
            if ($varName -notin @('PROCESSOR_ARCHITECTURE', 'PROCESSOR_IDENTIFIER', 'PROCESSOR_LEVEL', 'PROCESSOR_REVISION', 'NUMBER_OF_PROCESSORS')) {
                [Environment]::SetEnvironmentVariable($varName, $varValue, 'Process')
            }
        }
    }

    Write-Host "Environment variables updated from RSVars" -ForegroundColor Green
} finally {
    # Clean up temp file
    if (Test-Path $tempBatch) {
        Remove-Item $tempBatch -Force
    }
}

# Check if MSBuild exists
if (-not (Test-Path $MSBuildPath)) {
    Write-Host "Error: MSBuild not found at $MSBuildPath" -ForegroundColor Red
    exit 1
}

# Check if project file exists
if (-not (Test-Path $ProjectPath)) {
    Write-Host "Error: Project file not found at $ProjectPath" -ForegroundColor Red
    exit 1
}

try {
    Write-Host "Building project: $ProjectPath" -ForegroundColor Cyan

    # Build parameters - parse the build arguments string into an array
    $buildArgsArray = $BuildArguments -split ' (?=(?:[^"]|"[^"]*")*$)' | Where-Object { $_ -ne '' }

    # Combine project path with build arguments
    $allBuildArgs = @($ProjectPath) + $buildArgsArray

    & $MSBuildPath @allBuildArgs

    if ($LASTEXITCODE -eq 0) {
        Write-Host ""
        Write-Host "++++++++++++++++++++++++++++++++SUCCESS+++++++++++++++++++++++++++++++" -ForegroundColor Green
        Write-Host -NoNewline "---" -ForegroundColor Green; Write-Host " Delphi Utils - $FileName" -ForegroundColor Yellow
        Write-Host -NoNewline "---" -ForegroundColor Green; Write-Host " Action: $ActionDescription" -ForegroundColor Yellow
        Write-Host -NoNewline "---" -ForegroundColor Green; Write-Host " Path: $PathDescription" -ForegroundColor Yellow
        Write-Host -NoNewline "---" -ForegroundColor Green; Write-Host " Compiler: $CompilerName" -ForegroundColor Yellow
        Write-Host "++++++++++++++++++++++++++++++++SUCCESS+++++++++++++++++++++++++++++++" -ForegroundColor Green
        Write-Host ""
    } else {
        Write-Host ""
        Write-Host "++++++++++++++++++++++++++++++++FAILED++++++++++++++++++++++++++++++++" -ForegroundColor Red
        Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Delphi Utils - $FileName" -ForegroundColor Yellow
        Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Action: $ActionDescription" -ForegroundColor Yellow
        Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Path: $PathDescription" -ForegroundColor Yellow
        Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Compiler: $CompilerName" -ForegroundColor Yellow
        Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Exit Code: $LASTEXITCODE" -ForegroundColor Yellow
        Write-Host "++++++++++++++++++++++++++++++++FAILED++++++++++++++++++++++++++++++++" -ForegroundColor Red
        Write-Host ""
        exit $LASTEXITCODE
    }
} catch {
    Write-Host ""
    Write-Host "++++++++++++++++++++++++++++++++ERROR++++++++++++++++++++++++++++++++" -ForegroundColor Red
    Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Delphi Utils - $FileName" -ForegroundColor Yellow
    Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Action: $ActionDescription" -ForegroundColor Yellow
    Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Path: $PathDescription" -ForegroundColor Yellow
    Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Compiler: $CompilerName" -ForegroundColor Yellow
    Write-Host -NoNewline "---" -ForegroundColor Red; Write-Host " Error: $($_.Exception.Message)" -ForegroundColor Yellow
    Write-Host "++++++++++++++++++++++++++++++++ERROR++++++++++++++++++++++++++++++++" -ForegroundColor Red
    Write-Host ""
    exit 1
}
