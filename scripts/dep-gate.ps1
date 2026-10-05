# Phase 0 dependency gate. The crates that define the Service/Layer graph (http, tower,
# rustls, hyper, tokio, axum-core) must each resolve to exactly one version: two majors in
# the graph produce incompatible trait bounds that look like a bug in our own code.
#
# Deliberately NOT checked: base64, sha2 and digest, which sqlx vendors at older majors
# internally, and tower-http, of which reqwest keeps a private 0.6 copy for its own
# decompression layers. Neither crosses our API surface.
$ErrorActionPreference = 'Stop'
$critical = @('http', 'tower', 'rustls', 'hyper', 'tokio', 'axum-core')
$tree = cargo tree --edges normal --prefix none --format '{p}' 2>$null
$failed = $false

foreach ($name in $critical) {
    $versions = $tree |
        Select-String -Pattern "^$name v" |
        ForEach-Object { ($_.Line -split ' ')[1] } |
        Sort-Object -Unique
    if ($versions.Count -gt 1) {
        Write-Output "FAIL $name resolves to $($versions -join ', ')"
        $failed = $true
    } else {
        Write-Output "ok   $name $versions"
    }
}

# aws-lc-rs needs cmake and NASM to build and defeats a static musl link; ring must be the
# only rustls crypto provider in the graph.
if ($tree | Select-String -Pattern '^aws-lc-(rs|sys) v' -Quiet) {
    Write-Output 'FAIL aws-lc-rs is in the dependency graph; rustls must use ring only'
    $failed = $true
} else {
    Write-Output 'ok   rustls crypto provider is ring only'
}

if ($failed) { exit 1 }
Write-Output 'dependency gate passed'
