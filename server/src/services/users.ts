import type { Deps } from '../context'
import { decrypt, encrypt } from '../crypto'
import type { UserRow } from '../db'
import { IntraError, type IntraMe, type IntraTokens } from '../intra'

/** The user's stored 42 grant is gone or revoked: they must sign in on the web again. */
export class ReauthRequired extends Error {
  constructor() {
    super('42 authorization expired, sign in again on the web dashboard')
  }
}

function sealTokens(deps: Deps, userId: number, t: IntraTokens): string {
  return encrypt(deps.config.encryptionKey, JSON.stringify({ a: t.access_token, r: t.refresh_token }), `user:${userId}`)
}

function openTokens(deps: Deps, user: UserRow): { a: string; r: string | null } {
  return JSON.parse(decrypt(deps.config.encryptionKey, user.intra_token_enc, `user:${user.id}`))
}

export function upsertUser(deps: Deps, me: IntraMe, tokens: IntraTokens): UserRow {
  const { db } = deps
  const now = Date.now()
  const image = me.image?.versions?.small ?? me.image?.link ?? null
  return db.transaction(() => {
    const existing = db.query('SELECT id FROM users WHERE intra_id = ?').get(me.id) as { id: number } | null
    let id: number
    if (existing) {
      id = existing.id
    } else {
      // insert with a placeholder, then seal with the row id as AAD
      const r = db
        .query(`INSERT INTO users (intra_id, login, display_name, image_url, intra_token_enc, intra_expires_at, created_at, updated_at)
                VALUES (?, ?, ?, ?, '', 0, ?, ?)`)
        .run(me.id, me.login, me.displayname ?? null, image, now, now)
      id = Number(r.lastInsertRowid)
    }
    db.query(`UPDATE users SET login = ?, display_name = ?, image_url = ?, intra_token_enc = ?, intra_expires_at = ?, updated_at = ?
              WHERE id = ?`)
      .run(me.login, me.displayname ?? null, image, sealTokens(deps, id, tokens), tokens.expires_at, now, id)
    return db.query('SELECT * FROM users WHERE id = ?').get(id) as UserRow
  })()
}

const inflightRefresh = new Map<number, Promise<string>>()

/** Returns a valid 42 access token for the user, refreshing it (once, even under concurrency) when close to expiry. */
export async function intraAccessToken(deps: Deps, userId: number): Promise<string> {
  const user = deps.db.query('SELECT * FROM users WHERE id = ?').get(userId) as UserRow | null
  if (!user || !user.intra_token_enc) throw new ReauthRequired()
  const tokens = openTokens(deps, user)
  if (user.intra_expires_at - Date.now() > 60_000) return tokens.a
  if (!tokens.r) throw new ReauthRequired()

  let p = inflightRefresh.get(userId)
  if (!p) {
    p = (async () => {
      try {
        const fresh = await deps.intra.refresh(tokens.r!)
        if (!fresh.refresh_token) fresh.refresh_token = tokens.r
        deps.db.query('UPDATE users SET intra_token_enc = ?, intra_expires_at = ?, updated_at = ? WHERE id = ?')
          .run(sealTokens(deps, userId, fresh), fresh.expires_at, Date.now(), userId)
        return fresh.access_token
      } catch (err) {
        if (err instanceof IntraError && (err.status === 400 || err.status === 401)) {
          deps.db.query("UPDATE users SET intra_token_enc = '', intra_expires_at = 0 WHERE id = ?").run(userId)
          throw new ReauthRequired()
        }
        throw err
      } finally {
        inflightRefresh.delete(userId)
      }
    })()
    inflightRefresh.set(userId, p)
  }
  return p
}
