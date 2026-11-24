import type { Database } from 'bun:sqlite'
import type { Context, MiddlewareHandler } from 'hono'
import { getConnInfo } from 'hono/bun'
import type { Config } from './config'
import type { DeviceRow, UserRow } from './db'
import type { IntraClient } from './intra'
import type { TurnstileVerifier } from './turnstile'

export interface Deps {
  db: Database
  config: Config
  intra: IntraClient
  turnstile: TurnstileVerifier
}

export interface WebSession {
  idHash: string
  csrf: string
  user: UserRow
}

export type AppEnv = {
  Variables: {
    ip: string | null
    session: WebSession | null
    device: DeviceRow & { user: UserRow }
  }
}

export type Ctx = Context<AppEnv>

export function clientIpMiddleware(deps: Deps): MiddlewareHandler<AppEnv> {
  return async (c, next) => {
    c.set('ip', resolveIp(c, deps.config.trustProxy))
    await next()
  }
}

function resolveIp(c: Ctx, trust: Config['trustProxy']): string | null {
  if (trust === 'cloudflare') {
    const ip = c.req.header('cf-connecting-ip')
    if (ip) return ip
  } else if (trust === 'x-forwarded-for') {
    // the right-most entry is the one appended by our own proxy
    const xff = c.req.header('x-forwarded-for')
    const last = xff?.split(',').map((s) => s.trim()).filter(Boolean).pop()
    if (last) return last
  }
  try {
    return getConnInfo(c).remote.address ?? null
  } catch {
    return null // e.g. in-process tests (app.request) have no socket
  }
}

/** Simple fixed-window limiter, in memory. Good enough for a single instance; swap for Redis if you scale out. */
export function rateLimit(
  name: string,
  max: number,
  windowMs: number,
  key: (c: Ctx) => string | null = (c) => c.get('ip'),
): MiddlewareHandler<AppEnv> {
  const hits = new Map<string, { count: number; reset: number }>()
  return async (c, next) => {
    const k = key(c) ?? 'unknown'
    const now = Date.now()
    let entry = hits.get(k)
    if (!entry || entry.reset <= now) {
      if (hits.size > 10_000) for (const [hk, v] of hits) if (v.reset <= now) hits.delete(hk)
      entry = { count: 0, reset: now + windowMs }
      hits.set(k, entry)
    }
    entry.count++
    if (entry.count > max) {
      const retry = Math.ceil((entry.reset - now) / 1000)
      c.header('Retry-After', String(retry))
      return c.json({ error: 'rate_limited', error_description: `Too many requests (${name}), retry in ${retry}s` }, 429)
    }
    await next()
  }
}
