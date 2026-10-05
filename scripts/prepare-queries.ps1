# Regenerates the .sqlx query cache. This is the ONLY sanctioned way to do it.
#
# The cache records the type metadata of whatever the database looked like when it was
# built, not what migrations/ says. Preparing against a hand-edited development database
# bakes in metadata for a schema nobody else has: it compiles everywhere and then fails at
# runtime. So the database is always rebuilt from scratch first.
#
# Requires: docker compose -f docker-compose.dev.yml up -d
$ErrorActionPreference = 'Stop'

if (-not $env:DATABASE_URL) {
    $env:DATABASE_URL = 'postgres://rustle:rustle@localhost:5432/rustle'
    Write-Output "DATABASE_URL not set, using $env:DATABASE_URL"
}

# .cargo/config.toml pins SQLX_OFFLINE=true for everyone else; this one task needs it off.
$env:SQLX_OFFLINE = 'false'

sqlx database drop -y
sqlx database create
sqlx migrate run

# No --all-targets: tests deliberately avoid the query! macros and use unchecked
# sqlx::query for fixtures, which keeps the cache small and the tests honest.
cargo sqlx prepare

Write-Output 'query cache regenerated in .sqlx/'
