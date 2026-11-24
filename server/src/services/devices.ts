import type { Database } from 'bun:sqlite'
import type { Config } from '../config'
import { generateUserCode, hashToken, randomToken } from '../crypto'
import { audit, type DeviceRequestRow, type DeviceRow, type UserRow } from '../db'

export interface TokenPair {
  access_token: string
  refresh_token: string
  token_type: 'Bearer'
  expires_in: number
  device_id: string
}

export type OAuthError =
  | 'authorization_pending'
  | 'slow_down'
  | 'access_denied'
  | 'expired_token'
  | 'invalid_grant'
  | 'invalid_request'

export class DeviceFlowError extends Error {
  constructor(public code: OAuthError, message: string) {
    super(message)
  }
}

/** Errors are returned from transactions (not thrown) so side effects like rate tracking or revocation are committed. */
function unwrap(r: TokenPair | DeviceFlowError): TokenPair {
  if (r instanceof DeviceFlowError) throw r
  return r
}

const DEVICE_COLUMNS = `id, user_id, name, platform, created_at, last_seen_at, last_ip,
  access_expires_at, refresh_expires_at, revoked_at, revoked_reason`

/** Strip control characters and clamp length for user-supplied labels. */
export function cleanLabel(input: unknown, fallback: string, max = 64): string {
  if (typeof input !== 'string') return fallback
  const s = input.replace(/[\p{Cc}\p{Cf}]/gu, '').replace(/\s+/g, ' ').trim().slice(0, max)
  return s || fallback
}

export function createDeviceRequest(db: Database, cfg: Config, name: string, platform: string | null, ip: string | null) {
  const deviceCode = randomToken(32, 'ftgd_')
  const now = Date.now()
  // user_code is UNIQUE; retry on the (astronomically unlikely) collision
  for (let attempt = 0; ; attempt++) {
    const userCode = generateUserCode()
    try {
      db.query(`INSERT INTO device_requests
                (device_code_hash, user_code, name, platform, ip, status, poll_interval, created_at, expires_at)
                VALUES (?, ?, ?, ?, ?, 'pending', ?, ?, ?)`)
        .run(hashToken(deviceCode), userCode, name, platform, ip, cfg.devicePollIntervalS, now, now + cfg.deviceCodeTtlMs)
      return {
        device_code: deviceCode,
        user_code: userCode,
        verification_uri: `${cfg.publicUrl}/link`,
        verification_uri_complete: `${cfg.publicUrl}/link?code=${encodeURIComponent(userCode)}`,
        expires_in: Math.floor(cfg.deviceCodeTtlMs / 1000),
        interval: cfg.devicePollIntervalS,
      }
    } catch (err) {
      if (attempt >= 3) throw err
    }
  }
}

export function findPendingRequest(db: Database, userCode: string): DeviceRequestRow | null {
  const row = db.query('SELECT * FROM device_requests WHERE user_code = ?').get(userCode) as DeviceRequestRow | null
  if (!row || row.status !== 'pending' || row.expires_at < Date.now()) return null
  return row
}

export function decideDeviceRequest(db: Database, userCode: string, userId: number, approve: boolean, ip: string | null): DeviceRequestRow | null {
  return db.transaction(() => {
    const row = findPendingRequest(db, userCode)
    if (!row) return null
    db.query('UPDATE device_requests SET status = ?, user_id = ? WHERE device_code_hash = ?')
      .run(approve ? 'approved' : 'denied', userId, row.device_code_hash)
    audit(db, userId, approve ? 'device_approved' : 'device_denied', `${row.name} (request from ${row.ip ?? '?'})`, ip)
    return row
  })()
}

function issueTokens(db: Database, cfg: Config, deviceId: string, prevRefreshHash: string | null): TokenPair {
  const access = randomToken(32, 'ftga_')
  const refresh = randomToken(32, 'ftgr_')
  const now = Date.now()
  db.query(`UPDATE devices SET access_hash = ?, access_expires_at = ?, refresh_hash = ?, refresh_prev_hash = ?, refresh_expires_at = ?
            WHERE id = ?`)
    .run(hashToken(access), now + cfg.accessTokenTtlMs, hashToken(refresh), prevRefreshHash, now + cfg.refreshTokenTtlMs, deviceId)
  return {
    access_token: access,
    refresh_token: refresh,
    token_type: 'Bearer',
    expires_in: Math.floor(cfg.accessTokenTtlMs / 1000),
    device_id: deviceId,
  }
}

/** RFC 8628 §3.4 token polling. */
export function pollDeviceCode(db: Database, cfg: Config, deviceCode: string, ip: string | null): TokenPair {
  return unwrap(db.transaction((): TokenPair | DeviceFlowError => {
    const now = Date.now()
    const req = db.query('SELECT * FROM device_requests WHERE device_code_hash = ?')
      .get(hashToken(deviceCode)) as DeviceRequestRow | null
    if (!req || req.status === 'consumed') return new DeviceFlowError('invalid_grant', 'Unknown or already used device code')
    if (req.status === 'denied') return new DeviceFlowError('access_denied', 'The request was denied')
    if (req.expires_at < now) return new DeviceFlowError('expired_token', 'The device code expired, start again')

    if (req.status === 'pending') {
      const tooFast = req.last_poll_at !== null && now - req.last_poll_at < req.poll_interval * 1000
      db.query('UPDATE device_requests SET last_poll_at = ?, poll_interval = ? WHERE device_code_hash = ?')
        .run(now, tooFast ? req.poll_interval + 5 : req.poll_interval, req.device_code_hash)
      if (tooFast) return new DeviceFlowError('slow_down', `Poll at most every ${req.poll_interval + 5}s`)
      return new DeviceFlowError('authorization_pending', 'Waiting for the user to approve this device')
    }

    // approved → mint the device and its first token pair, exactly once
    const deviceId = crypto.randomUUID()
    db.query(`INSERT INTO devices (id, user_id, name, platform, created_at, last_seen_at, last_ip) VALUES (?, ?, ?, ?, ?, ?, ?)`)
      .run(deviceId, req.user_id, req.name, req.platform, now, now, ip)
    db.query("UPDATE device_requests SET status = 'consumed' WHERE device_code_hash = ?").run(req.device_code_hash)
    audit(db, req.user_id, 'device_linked', req.name, ip)
    return issueTokens(db, cfg, deviceId, null)
  })())
}

/**
 * Refresh-token rotation with replay detection: every refresh returns a new refresh token.
 * Presenting a refresh token that was already rotated means it was copied — revoke the device.
 */
export function refreshDeviceTokens(db: Database, cfg: Config, refreshToken: string, ip: string | null): TokenPair {
  return unwrap(db.transaction((): TokenPair | DeviceFlowError => {
    const h = hashToken(refreshToken)
    const now = Date.now()
    const dev = db.query('SELECT id, user_id, name, revoked_at, refresh_expires_at FROM devices WHERE refresh_hash = ?')
      .get(h) as Pick<DeviceRow, 'id' | 'user_id' | 'name' | 'revoked_at' | 'refresh_expires_at'> | null
    if (!dev) {
      const replayed = db.query('SELECT id, user_id, name FROM devices WHERE refresh_prev_hash = ? AND revoked_at IS NULL')
        .get(h) as Pick<DeviceRow, 'id' | 'user_id' | 'name'> | null
      if (replayed) {
        revokeDevice(db, replayed.user_id, replayed.id, 'refresh_token_reuse', ip)
        audit(db, replayed.user_id, 'refresh_token_reuse', `${replayed.name}: device revoked`, ip)
      }
      return new DeviceFlowError('invalid_grant', 'Invalid refresh token')
    }
    if (dev.revoked_at) return new DeviceFlowError('invalid_grant', 'Device was revoked')
    if ((dev.refresh_expires_at ?? 0) < now) return new DeviceFlowError('invalid_grant', 'Refresh token expired, link the device again')
    db.query('UPDATE devices SET last_seen_at = ?, last_ip = ? WHERE id = ?').run(now, ip, dev.id)
    return issueTokens(db, cfg, dev.id, h)
  })())
}

/** Resolves a bearer access token to its (non-revoked, non-expired) device and owner. */
export function authenticateAccessToken(db: Database, token: string): (DeviceRow & { user: UserRow }) | null {
  const dev = db.query(`SELECT ${DEVICE_COLUMNS} FROM devices WHERE access_hash = ? AND revoked_at IS NULL AND access_expires_at > ?`)
    .get(hashToken(token), Date.now()) as DeviceRow | null
  if (!dev) return null
  const user = db.query('SELECT * FROM users WHERE id = ?').get(dev.user_id) as UserRow
  return { ...dev, user }
}

export function touchDevice(db: Database, dev: DeviceRow, ip: string | null) {
  const now = Date.now()
  // avoid a write on every request
  if (!dev.last_seen_at || now - dev.last_seen_at > 60_000 || dev.last_ip !== ip)
    db.query('UPDATE devices SET last_seen_at = ?, last_ip = ? WHERE id = ?').run(now, ip, dev.id)
}

export function listDevices(db: Database, userId: number): DeviceRow[] {
  return db.query(`SELECT ${DEVICE_COLUMNS} FROM devices WHERE user_id = ? AND revoked_at IS NULL ORDER BY last_seen_at DESC`)
    .all(userId) as DeviceRow[]
}

/** Revokes a device owned by `userId`. Returns false if it does not exist / is not theirs. */
export function revokeDevice(db: Database, userId: number, deviceId: string, reason: string, ip: string | null): boolean {
  const r = db.query(`UPDATE devices SET revoked_at = ?, revoked_reason = ?, access_hash = NULL, refresh_hash = NULL
                      WHERE id = ? AND user_id = ? AND revoked_at IS NULL`)
    .run(Date.now(), reason, deviceId, userId)
  if (r.changes > 0 && reason !== 'refresh_token_reuse') audit(db, userId, 'device_revoked', `${deviceId} (${reason})`, ip)
  return r.changes > 0
}

export function revokeAllDevices(db: Database, userId: number, ip: string | null): number {
  const r = db.query(`UPDATE devices SET revoked_at = ?, revoked_reason = 'revoke_all', access_hash = NULL, refresh_hash = NULL
                      WHERE user_id = ? AND revoked_at IS NULL`).run(Date.now(), userId)
  audit(db, userId, 'devices_revoked_all', `${r.changes} device(s)`, ip)
  return r.changes
}

export function renameDevice(db: Database, userId: number, deviceId: string, name: string): boolean {
  return db.query('UPDATE devices SET name = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL')
    .run(name, deviceId, userId).changes > 0
}

export function publicDevice(d: DeviceRow, currentId?: string) {
  return {
    id: d.id,
    name: d.name,
    platform: d.platform,
    created_at: new Date(d.created_at).toISOString(),
    last_seen_at: d.last_seen_at ? new Date(d.last_seen_at).toISOString() : null,
    last_ip: d.last_ip,
    current: d.id === currentId,
  }
}
