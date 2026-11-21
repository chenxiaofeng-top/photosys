#Requires -Version 7.0
param(
    [Parameter(Mandatory)] [string] $Runner,
    [Parameter(Mandatory)] [string] $OutputDirectory,
    [string] $Test = 'library::index_benchmark::benchmark_index_pipeline'
)

$ErrorActionPreference = 'Stop'
$runnerPath = (Resolve-Path -LiteralPath $Runner).Path
$outputPath = (Resolve-Path -LiteralPath $OutputDirectory).Path
$utf8 = [System.Text.UTF8Encoding]::new($false)
$start = [System.Diagnostics.ProcessStartInfo]::new($runnerPath)
foreach ($argument in @('--exact', $Test, '--ignored', '--nocapture', '--test-threads=1')) {
    $start.ArgumentList.Add($argument)
}
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$start.StandardOutputEncoding = $utf8
$start.StandardErrorEncoding = $utf8

$log = [System.IO.StreamWriter]::new((Join-Path $outputPath 'benchmark.log'), $false, $utf8)
$samples = [System.IO.StreamWriter]::new((Join-Path $outputPath 'resources.csv'), $false, $utf8)
$log.AutoFlush = $true
$samples.AutoFlush = $true
$samples.WriteLine('elapsed_seconds,index_trial,process_cpu_seconds,working_set_mib,gpu_percent,gpu_memory_mib')
$gpuCommand = Get-Command nvidia-smi -CommandType Application -ErrorAction SilentlyContinue
$culture = [Globalization.CultureInfo]::InvariantCulture
$trial = 0
$clock = [Diagnostics.Stopwatch]::StartNew()
$process = [Diagnostics.Process]::Start($start)
try {
    $line = $process.StandardOutput.ReadLineAsync()
    $errors = $process.StandardError.ReadToEndAsync()
    while ($null -ne $line -or !$process.HasExited) {
        while ($null -ne $line -and $line.IsCompleted) {
            $message = $line.GetAwaiter().GetResult()
            if ($null -eq $message) {
                $line = $null
                break
            }
            $log.WriteLine($message)
            Write-Output $message
            if ($message -match '^index_started trial=(\d+)$') {
                $trial = [int]$Matches[1]
            } elseif ($message -match '^index_finished trial=') {
                $trial = 0
            }
            $line = $process.StandardOutput.ReadLineAsync()
        }
        if (!$process.HasExited) {
            $gpuSample = ','
            if ($gpuCommand) {
                $reading = & $gpuCommand.Source '--query-gpu=utilization.gpu,memory.used' '--format=csv,noheader,nounits' |
                    Select-Object -First 1
                if ($reading -match '^\s*(\d+)\s*,\s*(\d+)\s*$') {
                    $gpuSample = "$($Matches[1]),$($Matches[2])"
                }
            }
            try {
                $process.Refresh()
                $samples.WriteLine([string]::Format(
                    $culture, '{0:F3},{1},{2:F3},{3:F1},{4}',
                    $clock.Elapsed.TotalSeconds, $trial, $process.TotalProcessorTime.TotalSeconds,
                    $process.WorkingSet64 / 1MB, $gpuSample
                ))
            } catch [InvalidOperationException] {
                if (!$process.HasExited) { throw }
            }
            Start-Sleep -Milliseconds 350
        } elseif ($null -ne $line) {
            Start-Sleep -Milliseconds 10
        }
    }
    $stderr = $errors.GetAwaiter().GetResult()
    if ($stderr) {
        $log.WriteLine($stderr)
        Write-Output $stderr
    }
    exit $process.ExitCode
} finally {
    $process.Dispose()
    $samples.Dispose()
    $log.Dispose()
}
