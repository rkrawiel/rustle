-- Rustle baseline schema.
--
-- This is the single pre-release baseline: the whole schema in one file, so that
-- mid-project migration churn does not repeatedly invalidate the sqlx query cache.
-- Once applied it is IMMUTABLE -- sqlx checksums every migration and refuses to start if
-- an applied file changes. Later changes go in 0002_*.sql and onwards.
--
-- Rustle is single-user, but every owned table carries user_id so that multi-user becomes
-- a later migration rather than a rewrite. Nothing here assumes more than one row in users.

create table users (
    id              bigserial primary key,
    username        text        not null unique,
    password_hash   text        not null,
    theme           text        not null default 'system'
                                check (theme in ('system', 'light', 'dark')),
    entries_per_page integer    not null default 50
                                check (entries_per_page between 10 and 200),
    -- 0 means keep entries forever, and is the default.
    retention_days  integer     not null default 0 check (retention_days >= 0),
    created_at      timestamptz not null default now()
);

-- The cookie carries 32 random bytes; only their SHA-256 digest is stored, so a database
-- dump is not a set of live sessions.
create table sessions (
    id_hash      bytea       primary key,
    user_id      bigint      not null references users (id) on delete cascade,
    csrf_token   text        not null,
    created_at   timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    expires_at   timestamptz not null
);

create index sessions_expires_at_idx on sessions (expires_at);

create table categories (
    id         bigserial primary key,
    user_id    bigint      not null references users (id) on delete cascade,
    title      text        not null check (length(btrim(title)) > 0),
    created_at timestamptz not null default now(),
    unique (user_id, title)
);

create table feeds (
    id          bigserial primary key,
    user_id     bigint not null references users (id) on delete cascade,
    -- restrict, not cascade: deleting a category must not silently delete its feeds and
    -- every entry in them. The UI asks the user to move or delete the feeds first.
    category_id bigint not null references categories (id) on delete restrict,
    title       text   not null check (length(btrim(title)) > 0),
    feed_url    text   not null,
    site_url    text,

    -- Conditional GET state, echoed back as If-None-Match / If-Modified-Since.
    etag          text,
    last_modified text,

    -- Scheduling and health. next_check_at is the single input to the poller; it is also
    -- the lease column, so a claimed feed is invisible to a second poller.
    next_check_at          timestamptz not null default now(),
    check_interval_seconds integer     not null check (check_interval_seconds > 0),
    last_checked_at        timestamptz,
    last_success_at        timestamptz,
    -- Counts health failures only. A 429 or a 304 never increments it: treating rate
    -- limiting as ill-health would back a throttled feed off exponentially and eventually
    -- disable it, which is the wrong outcome.
    failure_count     integer not null default 0 check (failure_count >= 0),
    rate_limited_until timestamptz,
    last_error        text,
    disabled_at       timestamptz,

    created_at timestamptz not null default now(),
    unique (user_id, feed_url)
);

-- The poller's due-set. Partial, so disabled feeds never enter the scan.
create index feeds_due_idx on feeds (next_check_at) where disabled_at is null;
create index feeds_user_category_idx on feeds (user_id, category_id);

create table entries (
    id      bigserial primary key,
    -- Denormalised from feeds on purpose: without it every list query joins feeds to
    -- filter by user and loses the single-index ORDER BY published_at DESC LIMIT n.
    user_id bigint not null references users (id) on delete cascade,
    feed_id bigint not null references feeds (id) on delete cascade,

    -- Hash of the feed's own id, else the link, else title + published_at. Feeds with
    -- missing, empty or rotating guids are common, and getting this wrong either
    -- duplicates every entry on every poll or silently drops new ones.
    guid_hash bytea not null,

    title  text not null,
    url    text,
    author text,
    -- Sanitized once at ingest, never at render.
    content text not null default '',
    -- The same pass with tags stripped. The search vector is built from this, not from
    -- the HTML, or queries would match on "div" and "href".
    content_text text not null default '',
    -- Some feeds rewrite content on every fetch (ad tokens, timestamps). Updates compare
    -- this and skip the write when nothing really changed, so read state does not thrash.
    content_hash bytea not null,

    -- NOT NULL, defaulting to fetch time: ORDER BY ... DESC is NULLS FIRST, so a nullable
    -- column would silently add a sort node to every list query.
    published_at timestamptz not null,
    created_at   timestamptz not null default now(),

    -- Null means unread / unstarred. Timestamps rather than booleans so the starred view
    -- can order by when it was starred.
    read_at    timestamptz,
    starred_at timestamptz,

    -- The two-argument to_tsvector is mandatory here: the one-argument form is not
    -- IMMUTABLE and Postgres rejects it in a generated column.
    search tsvector generated always as (
        setweight(to_tsvector('english', coalesce(title, '')), 'A') ||
        setweight(to_tsvector('english', coalesce(content_text, '')), 'B')
    ) stored,

    unique (feed_id, guid_hash)
);

-- One index per access path. The (published_at desc, id desc) tail matches the keyset
-- comparator used by both list pagination and entry prev/next navigation.
create index entries_user_pub_idx on entries (user_id, published_at desc, id desc);
create index entries_user_unread_idx on entries (user_id, published_at desc, id desc)
    where read_at is null;
create index entries_user_starred_idx on entries (user_id, starred_at desc, id desc)
    where starred_at is not null;
create index entries_feed_pub_idx on entries (feed_id, published_at desc, id desc);
create index entries_feed_unread_idx on entries (feed_id, published_at desc, id desc)
    where read_at is null;
-- Sidebar unread counts: index-only over the unread rows, which do not grow with history.
create index entries_unread_counts_idx on entries (user_id, feed_id) where read_at is null;
create index entries_search_idx on entries using gin (search);
