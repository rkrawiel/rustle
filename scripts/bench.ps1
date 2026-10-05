# Measures p95 server response time for a list page with 10,000 entries in the database,
# per the brief's performance budget. Seeds directly with SQL rather than through ingest,
# since this is about the query planner against realistic row counts, not about fetching;
# signs in through the real /setup flow rather than hand-rolling a session row, since that
# is one SHA-256 digest away from just being a second, parallel implementation of login.
#
# Requires: docker compose -f docker-compose.dev.yml up -d
$ErrorActionPreference = 'Stop'

if (-not $env:DATABASE_URL) {
    $env:DATABASE_URL = 'postgres://rustle:rustle@localhost:5432/rustle'
}
$env:SQLX_OFFLINE = 'false'

Write-Output 'Resetting the database...'
sqlx database drop -y
sqlx database create
sqlx migrate run

Write-Output 'Building the release binary...'
cargo build --release

$port = 18080
$setupToken = 'bench-setup-token'
$env:RUSTLE_LISTEN_ADDR = "127.0.0.1:$port"
$env:RUSTLE_BASE_URL = "http://127.0.0.1:$port"
$env:RUSTLE_COOKIE_SECURE = 'false'
$env:RUSTLE_SETUP_TOKEN = $setupToken

$proc = Start-Process -FilePath '.\target\release\rustle.exe' -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 1

try {
    Add-Type -AssemblyName System.Net.Http
    $handler = New-Object System.Net.Http.HttpClientHandler
    $handler.CookieContainer = New-Object System.Net.CookieContainer
    $handler.AllowAutoRedirect = $false
    $client = New-Object System.Net.Http.HttpClient($handler)
    $base = "http://127.0.0.1:$port"

    Write-Output 'Signing up...'
    $form = [System.Collections.Generic.Dictionary[string, string]]::new()
    $form['setup_token'] = $setupToken
    $form['username'] = 'bench'
    $form['password'] = 'correct horse battery staple'
    $content = New-Object System.Net.Http.FormUrlEncodedContent($form)
    $request = New-Object System.Net.Http.HttpRequestMessage('POST', "$base/setup")
    $request.Content = $content
    $request.Headers.Add('Sec-Fetch-Site', 'same-origin')
    $setup = $client.SendAsync($request).GetAwaiter().GetResult()
    if ([int]$setup.StatusCode -ne 303) {
        throw "setup did not redirect: $($setup.StatusCode)"
    }

    # Signing up creates only the user; a category exists only once something has
    # subscribed to a feed through the app (Phase 4's `find_or_create`), so both are
    # inserted directly here rather than relied on. Piped over stdin rather than passed as
    # a `-c` argument: quoting a multi-word string through PowerShell -> docker exec -> the
    # container's own shell loses track of itself, and `-t -A` stops suppressing the
    # command tag once that happens.
    # Wrapped in a CTE so the whole statement is one SELECT as far as psql's command tag
    # is concerned ("SELECT 1", which `-t -A` does suppress) rather than an INSERT (whose
    # "INSERT 0 1" completion tag `-t` does not).
    $categoryId = ("with ins as (insert into categories (user_id, title) values (1, 'Uncategorized') returning id) select id from ins;" |
        docker exec -i rustle-dev-db-1 psql -t -A -U rustle -d rustle).Trim()
    $feedId = (@"
with ins as (
  insert into feeds (user_id, category_id, title, feed_url, check_interval_seconds, next_check_at)
  values (1, $categoryId, 'Bench feed', 'https://bench.invalid/feed', 3600, now() + interval '1 hour')
  returning id
)
select id from ins;
"@ | docker exec -i rustle-dev-db-1 psql -t -A -U rustle -d rustle).Trim()

    Write-Output 'Seeding 10,000 entries...'
    $sql = @"
insert into entries (user_id, feed_id, guid_hash, title, content, content_text, content_hash, published_at)
select 1, $feedId,
       decode(md5(i::text), 'hex'),
       'Entry ' || i,
       '<p>Body ' || i || '</p>',
       'Body ' || i,
       decode(md5('c' || i::text), 'hex'),
       now() - (i || ' minutes')::interval
from generate_series(1, 10000) as i;
"@
    $sql | docker exec -i rustle-dev-db-1 psql -v ON_ERROR_STOP=1 -U rustle -d rustle | Out-Null

    Write-Output 'Warming up...'
    $warmup = $client.GetAsync("$base/unread").GetAwaiter().GetResult()
    Write-Output "warm-up status: $($warmup.StatusCode)"
    if ([int]$warmup.StatusCode -ne 200) {
        throw "warm-up request failed: $($warmup.StatusCode)"
    }

    $n = 300
    Write-Output "Timing $n requests to /unread (10,000 entries in the database)..."
    $times = for ($i = 0; $i -lt $n; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $r = $client.GetAsync("$base/unread").GetAwaiter().GetResult()
        $sw.Stop()
        if ([int]$r.StatusCode -ne 200) {
            throw "unexpected status $($r.StatusCode) on request $i"
        }
        $sw.Elapsed.TotalMilliseconds
    }

    $sorted = $times | Sort-Object
    $p50 = $sorted[[int]([math]::Floor($sorted.Count * 0.50))]
    $p95 = $sorted[[int]([math]::Floor($sorted.Count * 0.95))]
    $p99 = $sorted[[int]([math]::Floor($sorted.Count * 0.99))]
    $max = $sorted[-1]

    Write-Output ''
    Write-Output ("n={0}  p50={1:N2} ms  p95={2:N2} ms  p99={3:N2} ms  max={4:N2} ms" -f `
        $sorted.Count, $p50, $p95, $p99, $max)
}
finally {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}
