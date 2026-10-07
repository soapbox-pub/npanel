// Append-only: never edit one that has shipped, add another. Existing
// databases have already run it, and record it by name.

pub const MIGRATIONS: &[(&str, &[&str])] = &[
    (
        "001_initial",
        &[
            r#"CREATE TABLE users (
        pubkey TEXT PRIMARY KEY,
        role TEXT NOT NULL CHECK (role IN ('admin', 'staff')),
        added_by TEXT,
        created_at INTEGER NOT NULL
      )"#,
            r#"CREATE TABLE hosts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        hostname TEXT NOT NULL UNIQUE,
        pubkey TEXT NOT NULL,
        kind INTEGER NOT NULL,
        identifier TEXT NOT NULL DEFAULT '',
        pinned_event_id TEXT,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      )"#,
            r#"CREATE INDEX hosts_site_idx ON hosts (pubkey, kind, identifier)"#,
            r#"CREATE TABLE host_grants (
        user_pubkey TEXT NOT NULL,
        host_id INTEGER NOT NULL REFERENCES hosts (id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (user_pubkey, host_id)
      )"#,
            r#"CREATE TABLE site_events (
        event_id TEXT PRIMARY KEY,
        pubkey TEXT NOT NULL,
        kind INTEGER NOT NULL,
        identifier TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        event TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending', 'fetching_blobs', 'ready', 'failed')),
        files_total INTEGER NOT NULL DEFAULT 0,
        files_ready INTEGER NOT NULL DEFAULT 0,
        error TEXT,
        fetched_at INTEGER NOT NULL,
        ready_at INTEGER
      )"#,
            r#"CREATE INDEX site_events_site_idx ON site_events (pubkey, kind, identifier, created_at DESC)"#,
            r#"CREATE INDEX site_events_status_idx ON site_events (status)"#,
            r#"CREATE TABLE site_files (
        event_id TEXT NOT NULL REFERENCES site_events (event_id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        sha256 TEXT NOT NULL,
        PRIMARY KEY (event_id, path)
      )"#,
            r#"CREATE INDEX site_files_sha_idx ON site_files (sha256)"#,
            r#"CREATE TABLE blobs (
        sha256 TEXT PRIMARY KEY,
        size INTEGER NOT NULL,
        seen_at INTEGER NOT NULL
      )"#,
            r#"CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at INTEGER NOT NULL
      )"#,
            r#"CREATE TABLE audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        actor_pubkey TEXT NOT NULL,
        action TEXT NOT NULL,
        target TEXT,
        old_value TEXT,
        new_value TEXT,
        created_at INTEGER NOT NULL
      )"#,
            r#"CREATE INDEX audit_log_created_idx ON audit_log (created_at DESC)"#,
            r#"CREATE TABLE deploy_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL,
        level TEXT NOT NULL,
        msg TEXT NOT NULL,
        fields TEXT,
        created_at INTEGER NOT NULL
      )"#,
            r#"CREATE INDEX deploy_log_event_idx ON deploy_log (event_id, id)"#,
        ],
    ),
    (
        "002_drop_allow_private_blob_servers",
        &[
            r#"DELETE FROM settings WHERE key = 'allow_private_blob_servers'"#,
        ],
    ),
    (
        "003_hosts_hidden",
        &[
            r#"ALTER TABLE hosts ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0"#,
        ],
    ),
    (
        "004_hosts_redirect_www",
        &[
            r#"ALTER TABLE hosts ADD COLUMN redirect_www INTEGER NOT NULL DEFAULT 1"#,
        ],
    ),
    (
        "005_hosts_spa",
        &[
            r#"ALTER TABLE hosts ADD COLUMN spa INTEGER NOT NULL DEFAULT 0"#,
        ],
    ),
    (
        "006_bunkers",
        &[
            r#"CREATE TABLE bunkers (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        slug TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,
        user_pubkey TEXT,
        upstream_pubkey TEXT,
        upstream_secret TEXT,
        upstream_relays TEXT NOT NULL DEFAULT '[]',
        key_fingerprint TEXT,
        status TEXT NOT NULL CHECK (status IN ('pending', 'connected', 'error')),
        error TEXT,
        last_ok_at INTEGER,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      )"#,
            r#"CREATE TABLE bunker_grants (
        user_pubkey TEXT NOT NULL,
        bunker_id INTEGER NOT NULL REFERENCES bunkers (id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (user_pubkey, bunker_id)
      )"#,
            r#"CREATE TABLE bunker_connections (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        bunker_id INTEGER NOT NULL REFERENCES bunkers (id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        signer_secret TEXT NOT NULL,
        signer_pubkey TEXT NOT NULL UNIQUE,
        client_pubkey TEXT,
        secret TEXT,
        relays TEXT NOT NULL DEFAULT '[]',
        permissions TEXT NOT NULL DEFAULT '[]',
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_used_at INTEGER,
        last_error TEXT
      )"#,
            r#"CREATE INDEX bunker_connections_bunker_idx ON bunker_connections (bunker_id)"#,
        ],
    ),
    (
        "007_drop_bunkers",
        &[
            r#"DROP TABLE IF EXISTS bunker_connections"#,
            r#"DROP TABLE IF EXISTS bunker_grants"#,
            r#"DROP TABLE IF EXISTS bunkers"#,
            r#"DELETE FROM settings WHERE key IN
        ('bunker_relays', 'bunker_request_timeout', 'bunker_max_relays', 'bunker_rate_limit')"#,
        ],
    ),
    (
        "008_hosts_csp",
        &[
            r#"ALTER TABLE hosts ADD COLUMN csp INTEGER"#,
        ],
    ),
    (
        "009_hosts_csp_default_on",
        &[
            r#"ALTER TABLE hosts ADD COLUMN csp_on INTEGER NOT NULL DEFAULT 1"#,
            r#"UPDATE hosts SET csp_on = 0 WHERE csp = 0"#,
            r#"ALTER TABLE hosts DROP COLUMN csp"#,
            r#"ALTER TABLE hosts RENAME COLUMN csp_on TO csp"#,
            r#"DELETE FROM settings WHERE key = 'nsite_csp'"#,
        ],
    ),
    (
        "010_tor_onions",
        &[
            r#"CREATE TABLE onion_services (
        host_id INTEGER PRIMARY KEY REFERENCES hosts (id) ON DELETE CASCADE,
        address TEXT NOT NULL UNIQUE,
        secret_key TEXT NOT NULL,
        created_at INTEGER NOT NULL
      )"#,
        ],
    ),
    (
        "011_nip05_names",
        &[
            r#"CREATE TABLE nip05_names (
        host_id INTEGER NOT NULL REFERENCES hosts (id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        pubkey TEXT NOT NULL,
        relays TEXT NOT NULL DEFAULT '[]',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (host_id, name)
      )"#,
        ],
    ),
    (
        "012_blob_variant_sizes",
        &[
            r#"ALTER TABLE blobs ADD COLUMN br_size INTEGER"#,
            r#"ALTER TABLE blobs ADD COLUMN gzip_size INTEGER"#,
        ],
    ),
    (
        "013_site_events_ready_idx",
        &[
            r#"CREATE INDEX site_events_ready_idx
        ON site_events (pubkey, kind, identifier, created_at DESC)
        WHERE status = 'ready'"#,
        ],
    ),
    (
        "014_hosts_seo",
        &[
            r#"ALTER TABLE hosts ADD COLUMN seo INTEGER NOT NULL DEFAULT 1"#,
        ],
    ),
    (
        "015_hosts_merge_deployments",
        &[
            r#"ALTER TABLE hosts ADD COLUMN merge_deployments INTEGER NOT NULL DEFAULT 0"#,
        ],
    ),
    (
        "016_host_builds",
        &[
            r#"CREATE TABLE host_builds (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        host_id INTEGER NOT NULL REFERENCES hosts (id) ON DELETE CASCADE,
        kind TEXT NOT NULL CHECK (kind IN ('clearnet', 'onion')),
        origin TEXT NOT NULL,
        event_id TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('building', 'ready', 'failed')),
        error TEXT,
        last_modified INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        built_at INTEGER
      )"#,
            r#"CREATE INDEX host_builds_host_idx ON host_builds (host_id, kind, id DESC)"#,
            r#"CREATE TABLE host_build_files (
        build_id INTEGER NOT NULL REFERENCES host_builds (id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        sha256 TEXT NOT NULL,
        event_id TEXT,
        source_sha256 TEXT,
        PRIMARY KEY (build_id, path)
      )"#,
            r#"CREATE INDEX host_build_files_sha_idx ON host_build_files (sha256)"#,
        ],
    ),
    (
        "017_hosts_analytics",
        &[
            r#"ALTER TABLE hosts ADD COLUMN analytics INTEGER NOT NULL DEFAULT 0"#,
        ],
    ),
    (
        "018_audit_log_redact_secrets",
        &[
            r#"UPDATE audit_log
        SET old_value = CASE WHEN old_value IS NULL THEN NULL WHEN old_value = '""' THEN 'false' ELSE 'true' END,
            new_value = CASE WHEN new_value IS NULL THEN NULL WHEN new_value = '""' THEN 'false' ELSE 'true' END
        WHERE action = 'settings.update' AND target = 'tor_control_password'"#,
        ],
    ),
    (
        "019_relays_wss_only",
        &[
            r#"UPDATE settings
        SET value = (SELECT json_group_array(value) FROM json_each(settings.value) WHERE value NOT LIKE 'ws://%')
        WHERE key = 'relays'"#,
            r#"DELETE FROM settings WHERE key = 'relays' AND value = '[]'"#,
            r#"UPDATE nip05_names
        SET relays = (SELECT json_group_array(value) FROM json_each(nip05_names.relays) WHERE value NOT LIKE 'ws://%')
        WHERE relays LIKE '%"ws://%'"#,
        ],
    ),
    (
        "020_host_env",
        &[
            r#"CREATE TABLE host_env (
        host_id INTEGER NOT NULL REFERENCES hosts (id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        value TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (host_id, name)
      )"#,
        ],
    ),
    (
        "021_tor_embedded",
        &[
            r#"DELETE FROM settings WHERE key IN ('tor_control_url', 'tor_control_password')"#,
        ],
    ),
    (
        "022_certificates",
        &[
            r#"CREATE TABLE acme_accounts (
        directory TEXT PRIMARY KEY,
        credentials TEXT NOT NULL,
        email TEXT NOT NULL,
        created_at INTEGER NOT NULL
      )"#,
            r#"CREATE TABLE certificates (
        directory TEXT NOT NULL,
        hostname TEXT NOT NULL,
        chain TEXT,
        private_key TEXT,
        not_before INTEGER,
        not_after INTEGER,
        issued_at INTEGER,
        failures INTEGER NOT NULL DEFAULT 0,
        attempted_at INTEGER,
        error TEXT,
        PRIMARY KEY (directory, hostname)
      )"#,
        ],
    ),
    (
        "023_hosts_link_previews",
        &[
            r#"ALTER TABLE hosts ADD COLUMN link_previews INTEGER NOT NULL DEFAULT 0"#,
        ],
    ),
    (
        "024_hosts_errors",
        &[
            r#"ALTER TABLE hosts ADD COLUMN errors INTEGER NOT NULL DEFAULT 0"#,
        ],
    ),
    (
        "025_hosts_analytics_script",
        &[
            r#"ALTER TABLE hosts ADD COLUMN analytics_script INTEGER NOT NULL DEFAULT 1"#,
        ],
    ),
];
