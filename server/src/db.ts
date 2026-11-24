import { Database } from 'bun:sqlite'
import { mkdirSync } from 'node:fs'
import { dirname } from 'node:path'

const MIGRATIONS: string[] = [
  `
  CREATE TABLE users (
    id               INTEGER PRIMARY KEY,
    intra_id         INTEGER NOT NULL UNIQUE,
    login            TEXT    NOT NULL,
    display_name     TEXT,
    image_url        TEXT,
    intra_token_enc  TEXT    NOT NULL,   -- AES-GCM({access_token, refresh_token}), AAD = user id
    intra_expires_at INTEGER NOT NULL,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
  );

  CREATE TABLE web_sessions (
    id_hash    TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf       TEXT    NOT NULL,
    ip         TEXT,
    user_agent TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
  );

  CREATE TABLE oauth_states (
    state_hash TEXT PRIMARY KEY,
    next       TEXT    NOT NULL,
    expires_at INTEGER NOT NULL
  );

  -- RFC 8628-style pending device authorisations
  CREATE TABLE device_requests (
    device_code_hash TEXT PRIMARY KEY,
    user_code        TEXT    NOT NULL UNIQUE,
    name             TEXT    NOT NULL,
    platform         TEXT,
    ip               TEXT,
    status           TEXT    NOT NULL CHECK (status IN ('pending','approved','denied','consumed')),
    user_id          INTEGER REFERENCES users(id) ON DELETE CASCADE,
    poll_interval    INTEGER NOT NULL,
    last_poll_at     INTEGER,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL
  );

  CREATE TABLE devices (
    id                 TEXT PRIMARY KEY,
    user_id            INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name               TEXT    NOT NULL,
    platform           TEXT,
    created_at         INTEGER NOT NULL,
    last_seen_at       INTEGER,
    last_ip            TEXT,
    access_hash        TEXT UNIQUE,
    access_expires_at  INTEGER,
    refresh_hash       TEXT UNIQUE,
    refresh_prev_hash  TEXT,          -- previous refresh token, kept to detect replay
    refresh_expires_at INTEGER,
    revoked_at         INTEGER,
    revoked_reason     TEXT
  );
  CREATE INDEX devices_user ON devices(user_id);
  CREATE INDEX devices_refresh_prev ON devices(refresh_prev_hash);

  CREATE TABLE audit_log (
    id      INTEGER PRIMARY KEY,
    user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
    event   TEXT    NOT NULL,
    detail  TEXT,
    ip      TEXT,
    at      INTEGER NOT NULL
  );
  CREATE INDEX audit_user ON audit_log(user_id, at);
  `,
]

export function openDatabase(path: string): Database {
  if (path !== ':memory:') mkdirSync(dirname(path), { recursive: true })
  const db = new Database(path, { create: true, strict: true })
  db.exec('PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;')
  migrate(db)
  return db
}

function migrate(db: Database) {
  const { user_version } = db.query('PRAGMA user_version').get() as { user_version: number }
  for (let v = user_version; v < MIGRATIONS.length; v++) {
    db.transaction(() => {
      db.exec(MIGRATIONS[v]!)
      db.exec(`PRAGMA user_version = ${v + 1}`)
    })()
  }
}

/** Delete expired rows. Called periodically. */
export function purgeExpired(db: Database, now = Date.now()) {
  db.query('DELETE FROM oauth_states WHERE expires_at < ?').run(now)
  db.query('DELETE FROM web_sessions WHERE expires_at < ?').run(now)
  // keep consumed/denied requests a little longer for slow pollers, then drop
  db.query('DELETE FROM device_requests WHERE expires_at < ?').run(now - 3600_000)
  db.query('DELETE FROM audit_log WHERE at < ?').run(now - 90 * 24 * 3600_000)
}

export function audit(db: Database, userId: number | null, event: string, detail: string | null, ip: string | null) {
  db.query('INSERT INTO audit_log (user_id, event, detail, ip, at) VALUES (?, ?, ?, ?, ?)').run(
    userId, event, detail, ip, Date.now())
}

export interface UserRow {
  id: number
  intra_id: number
  login: string
  display_name: string | null
  image_url: string | null
  intra_token_enc: string
  intra_expires_at: number
}

export interface DeviceRow {
  id: string
  user_id: number
  name: string
  platform: string | null
  created_at: number
  last_seen_at: number | null
  last_ip: string | null
  access_expires_at: number | null
  refresh_expires_at: number | null
  revoked_at: number | null
  revoked_reason: string | null
}

export interface DeviceRequestRow {
  device_code_hash: string
  user_code: string
  name: string
  platform: string | null
  ip: string | null
  status: 'pending' | 'approved' | 'denied' | 'consumed'
  user_id: number | null
  poll_interval: number
  last_poll_at: number | null
  created_at: number
  expires_at: number
}
