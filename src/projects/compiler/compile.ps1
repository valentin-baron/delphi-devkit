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
  [Parameter(Mandatory = $true)]
  [string]$ProjectPath,

  [Parameter(Mandatory = $true)]
  [string]$RSVarsPath,

  [Parameter(Mandatory = $true)]
  [string]$MSBuildPath,

  [Parameter(Mandatory = $true)]
  [string]$FileName,

  [Parameter(Mandatory = $true)]
  [string]$ActionDescription,

  [Parameter(Mandatory = $true)]
  [string]$PathDescription,

  [Parameter(Mandatory = $true)]
  [string]$BuildArguments,

  [Parameter(Mandatory = $true)]
  [string]$CompilerName
)
function Test-FileLocked {
  param([string]$Path)
  if (-not (Test-Path $Path)) { return $false }
  try {
    $fs = [System.IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
    $fs.Close()
    return $false
  }
  catch {
    return $true
  }
}
function Get-CompilerObject {
  param ($line)

  # Regex breakdown:
  # ^(.*?)\((\d+)(?:,(\d+))?\):\s+        => file path, line, (optional column), colon
  # (.*?)\s+([A-Z]\d+):\s+                => any text (hint/warning/error text), code
  # (.*?)(?:\s+\[.*\])?$                  => message, ignore trailing [projectfile]
  $regex = '^(.*?)\((\d+)(?:,(\d+))?\):\s+(.*?)\s+([A-Z]\d+):\s+(.*?)(?:\s+\[.*\])?$'
  if ($line -match $regex) {
    return [PSCustomObject]@{
      FileName   = $matches[1] # C:\path\to\file.pas
      LineNumber = $matches[2] # (123)
      TypeText   = $matches[4] # Warning
      Code       = $matches[5] # W1234
      Message    = $matches[6]
    }
  }

}
function Format-Output {
  param ([string]$line)

  $object = Get-CompilerObject $line

  if ($object) {
    $color = 'Red'
    $type = 'ERROR'
    switch ($object.Code[0]) {
      'W' {
        $color = 'Yellow'
        $type = 'WARN'
      }
      'H' {
        $color = 'Green'
        $type = 'HINT'
      }
    }
    # get current formatted time:
    $timestamp = (Get-Date).ToString("T")
    # Convert Windows-Style path to URI:
    $outLine = "( $timestamp ) [$type] [$($object.Code)] $($object.FileName):$($object.LineNumber) - $($object.Message)"
    Write-Host $outLine -ForegroundColor $color
  } elseif ($line -ne "") {
    # If no match, just output the line as is
    Write-Host $line -ForegroundColor White
  }
}

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

function Draw-Header ([string]$header) {
  function Format-Line ([string]$text, [int]$totalWidth = 70) {
    $padding = $totalWidth - $text.Length - 2
    if ($padding -le 0) {
      return $text
    }
    $leftPadding = [math]::Floor($padding / 2)
    return " " + (" " * $leftPadding) + " $text"

  }
  Write-Host "╒══════════════════════════════════════════════════════════════════════╕"
  Write-Host( Format-Line "🅳🅳🅺 $header 🅳🅳🅺" 72)
  Write-Host( Format-Line "→ $FileName ←" )
  Write-Host( Format-Line "🗲 Action: $ActionDescription" )
  Write-Host( Format-Line "📂︎ Path: $PathDescription" 71 )
  Write-Host( Format-Line "🛠 Compiler: $CompilerName" )
  Write-Host "╘══════════════════════════════════════════════════════════════════════╛"
}

$success = $false

Draw-Header "Compile START"

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
}
finally {
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

  # Derive expected primary EXE output (best-effort). For Delphi projects the default is <ProjectName>.exe beside the .dproj
  $projectDir = [IO.Path]::GetDirectoryName($ProjectPath)
  $projectBase = [IO.Path]::GetFileNameWithoutExtension($ProjectPath)
  $expectedExe = Join-Path $projectDir ("$projectBase.exe")

  if (Test-Path $expectedExe) {
    if (Test-FileLocked -Path $expectedExe) {
      Write-Host "Target executable appears to be in use: $expectedExe" -ForegroundColor Red
      Write-Host "Close the running application before compiling." -ForegroundColor Yellow
      Write-Host "(You can add logic to auto-terminate it if desired.)" -ForegroundColor DarkYellow
      Write-Host ""; Draw-Header "Compile FAILED"; exit 1
    }
  }

  $buildStart = Get-Date

  # Build parameters - parse the build arguments string into an array
  $buildArgsArray = $BuildArguments -split ' (?=(?:[^"]|"[^"]*")*$)' | Where-Object { $_ -ne '' }

  # Combine project path with build arguments
  $allBuildArgs = @($ProjectPath) + $buildArgsArray

  & $MSBuildPath @allBuildArgs | ForEach-Object {
    Format-Output $_.Trim()
  }

  if ($LASTEXITCODE -ne 0) {
    Write-Host ""; Draw-Header "Compile FAILED"; exit $LASTEXITCODE
  }

  # If an expected exe exists, validate it was updated (helps catch silent skips / lock scenarios)
  if (Test-Path $expectedExe) {
    $exeInfo = Get-Item $expectedExe -ErrorAction SilentlyContinue
    if (-not $exeInfo -or $exeInfo.LastWriteTime -lt $buildStart) {
      Write-Host ""; Draw-Header "Compile FAILED - exe is locked"; exit 1
    }
  }

  Write-Host ""; Draw-Header "Compile SUCCESS"; Write-Host ""
}
catch {
  Write-Host ""
  Draw-Header "Compile FAILED"
  exit 1
}
