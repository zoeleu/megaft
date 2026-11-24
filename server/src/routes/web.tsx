import { Hono, type MiddlewareHandler } from 'hono'
import { deleteCookie, getCookie, setCookie } from 'hono/cookie'
import { redirectUri } from '../config'
import { rateLimit, type AppEnv, type Ctx, type Deps } from '../context'
import { hashToken, normalizeUserCode, randomToken, safeEqual } from '../crypto'
import { audit, type UserRow } from '../db'
import { INTRA_AUTHORIZE, type IntraMe } from '../intra'
import {
  cleanLabel, decideDeviceRequest, findPendingRequest, listDevices, renameDevice, revokeAllDevices, revokeDevice,
} from '../services/devices'
import { clearSummaryCache } from '../services/summary'
import { upsertUser } from '../services/users'
import { ConfirmLinkPage, CSS, DevicesPage, EnterCodePage, LoginPage, MessagePage } from '../views'

const STATE_TTL_MS = 10 * 60_000

/** Only allow same-site relative redirects ("/devices", "/link?code=…"), never "//evil" or "/\evil". */
export function safeNext(next: string | undefined | null): string {
  if (!next || !next.startsWith('/') || next.startsWith('//') || next.startsWith('/\\') || next.length > 200) return '/devices'
  return next
}

export function webRoutes(deps: Deps) {
  const { db, config } = deps
  const prefix = config.secureCookies ? '__Host-' : ''
  const SESSION_COOKIE = `${prefix}ftg_session`
  const STATE_COOKIE = `${prefix}ftg_oauth_state`
  const cookieBase = { path: '/', httpOnly: true, secure: config.secureCookies, sameSite: 'Lax' as const }

  const app = new Hono<AppEnv>()

  // Resolve the web session (if any) on every request.
  app.use('*', async (c, next) => {
    c.set('session', null)
    const raw = getCookie(c, SESSION_COOKIE)
    if (raw && raw.length < 100) {
      const idHash = hashToken(raw)
      const row = db.query(`SELECT s.csrf, s.expires_at, u.* FROM web_sessions s JOIN users u ON u.id = s.user_id WHERE s.id_hash = ?`)
        .get(idHash) as (UserRow & { csrf: string; expires_at: number }) | null
      if (row && row.expires_at > Date.now()) {
        const { csrf, expires_at: _e, ...user } = row
        c.set('session', { idHash, csrf, user })
      }
    }
    await next()
  })

  // CSRF: all state-changing requests need a same-origin Origin (when sent) and, once logged in, the session token.
  const csrf: MiddlewareHandler<AppEnv> = async (c, next) => {
    // Prefer Fetch Metadata (all current browsers); fall back to Origin for older clients.
    const site = c.req.header('sec-fetch-site')
    const origin = c.req.header('origin')
    const crossSite = site
      ? site !== 'same-origin' && site !== 'none'
      : origin !== undefined && origin !== new URL(config.publicUrl).origin
    if (crossSite) return c.text('Cross-site request blocked', 403)
    const s = c.get('session')
    if (s) {
      const body = await c.req.parseBody()
      const token = typeof body._csrf === 'string' ? body._csrf : ''
      if (!safeEqual(token, s.csrf)) return c.text('Invalid CSRF token, reload the page and try again', 403)
    }
    await next()
  }

  const requireSession: MiddlewareHandler<AppEnv> = async (c, next) => {
    if (!c.get('session')) {
      const next_ = c.req.method === 'GET' ? c.req.url.slice(new URL(c.req.url).origin.length) : '/devices'
      return c.redirect(`/login?next=${encodeURIComponent(safeNext(next_))}`)
    }
    await next()
  }

  app.get('/assets/app.css', (c) => {
    c.header('Cache-Control', 'public, max-age=3600')
    return c.body(CSS, 200, { 'Content-Type': 'text/css; charset=utf-8' })
  })

  app.get('/', (c) => c.redirect(c.get('session') ? '/devices' : '/login'))

  // ── Login: Turnstile → 42 OAuth ──────────────────────────────────────────
  app.get('/login', (c) => {
    const next = safeNext(c.req.query('next'))
    if (c.get('session')) return c.redirect(next)
    return c.html(<LoginPage siteKey={config.turnstileSiteKey} next={next} />)
  })

  app.post('/login', rateLimit('login', 10, 60_000), csrf, async (c) => {
    const body = await c.req.parseBody()
    const next = safeNext(typeof body.next === 'string' ? body.next : null)
    const token = typeof body['cf-turnstile-response'] === 'string' ? body['cf-turnstile-response'] : ''
    const result = await deps.turnstile.verify(token, c.get('ip'))
    if (!result.success)
      return c.html(<LoginPage siteKey={config.turnstileSiteKey} next={next} error="The captcha check failed, please try again." />, 400)

    const state = randomToken(32)
    db.query('INSERT INTO oauth_states (state_hash, next, expires_at) VALUES (?, ?, ?)')
      .run(hashToken(state), next, Date.now() + STATE_TTL_MS)
    // bind the state to this browser: the callback must present both the cookie and the query param
    setCookie(c, STATE_COOKIE, state, { ...cookieBase, maxAge: STATE_TTL_MS / 1000 })

    const url = new URL(INTRA_AUTHORIZE)
    url.searchParams.set('client_id', config.ftClientId)
    url.searchParams.set('redirect_uri', redirectUri(config))
    url.searchParams.set('response_type', 'code')
    url.searchParams.set('scope', 'public')
    url.searchParams.set('state', state)
    return c.redirect(url.toString(), 303)
  })

  app.get('/auth/callback', rateLimit('callback', 20, 60_000), async (c) => {
    const fail = (msg: string, status: 400 | 502 = 400) =>
      c.html(<MessagePage title="Sign-in failed" message={msg} error />, status)

    const state = c.req.query('state') ?? ''
    const cookieState = getCookie(c, STATE_COOKIE) ?? ''
    deleteCookie(c, STATE_COOKIE, cookieBase)
    if (c.req.query('error')) return fail('Authorisation was cancelled on the 42 Intra.')
    if (!state || !cookieState || !safeEqual(state, cookieState))
      return fail('Your sign-in session is missing or expired. Start again from the login page.')

    const row = db.query('DELETE FROM oauth_states WHERE state_hash = ? RETURNING next, expires_at')
      .get(hashToken(state)) as { next: string; expires_at: number } | null
    if (!row || row.expires_at < Date.now()) return fail('This sign-in link was already used or has expired.')

    const code = c.req.query('code')
    if (!code || code.length > 512) return fail('Missing authorisation code from the 42 Intra.')

    let user: UserRow
    try {
      const tokens = await deps.intra.exchangeCode(code, redirectUri(config))
      const me = await deps.intra.get<IntraMe>('/v2/me', tokens.access_token)
      user = upsertUser(deps, me, tokens)
    } catch (err) {
      console.error('oauth callback failed:', err instanceof Error ? err.message : err)
      return fail('Could not complete sign-in with the 42 Intra. Try again in a moment.', 502)
    }
    clearSummaryCache(user.id)

    // fresh session id on every login (no session fixation); drop any previous one
    const old = c.get('session')
    if (old) db.query('DELETE FROM web_sessions WHERE id_hash = ?').run(old.idHash)
    const sid = randomToken(32)
    const now = Date.now()
    db.query(`INSERT INTO web_sessions (id_hash, user_id, csrf, ip, user_agent, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)`)
      .run(hashToken(sid), user.id, randomToken(24), c.get('ip'), (c.req.header('user-agent') ?? '').slice(0, 200), now, now + config.sessionTtlMs)
    setCookie(c, SESSION_COOKIE, sid, { ...cookieBase, maxAge: config.sessionTtlMs / 1000 })
    audit(db, user.id, 'web_login', null, c.get('ip'))
    return c.redirect(safeNext(row.next))
  })

  app.post('/logout', csrf, (c) => {
    const s = c.get('session')
    if (s) db.query('DELETE FROM web_sessions WHERE id_hash = ?').run(s.idHash)
    deleteCookie(c, SESSION_COOKIE, cookieBase)
    return c.redirect('/login', 303)
  })

  // ── Device linking (user side of the device flow) ────────────────────────
  const linkLimiter = rateLimit('link', 30, 10 * 60_000)

  app.get('/link', requireSession, linkLimiter, (c) => {
    const s = c.get('session')!
    const raw = c.req.query('code')
    if (!raw) return c.html(<EnterCodePage user={s.user} csrf={s.csrf} />)
    const code = normalizeUserCode(raw)
    const req = code ? findPendingRequest(db, code) : null
    if (!req)
      return c.html(<EnterCodePage user={s.user} csrf={s.csrf} error="That code is invalid or has expired. Check the code on your computer, or start linking again." />, 404)
    return c.html(<ConfirmLinkPage user={s.user} csrf={s.csrf} req={req} currentIp={c.get('ip')} />)
  })

  app.post('/link', requireSession, linkLimiter, csrf, async (c) => {
    const s = c.get('session')!
    const body = await c.req.parseBody()
    const code = normalizeUserCode(typeof body.code === 'string' ? body.code : null)
    const approve = body.decision === 'approve'
    const req = code ? decideDeviceRequest(db, code, s.user.id, approve, c.get('ip')) : null
    if (!req)
      return c.html(<MessagePage title="Code expired" message="That code is invalid or has expired. Start linking again from the extension." user={s.user} csrf={s.csrf} error />, 404)
    return c.html(
      approve
        ? <MessagePage title="Device linked" message={`“${req.name}” is now linked to ${s.user.login}. You can return to your desktop.`} user={s.user} csrf={s.csrf} />
        : <MessagePage title="Request denied" message={`“${req.name}” was not linked.`} user={s.user} csrf={s.csrf} />,
    )
  })

  // ── Device management ────────────────────────────────────────────────────
  app.get('/devices', requireSession, (c) => {
    const s = c.get('session')!
    const activity = db.query('SELECT event, detail, ip, at FROM audit_log WHERE user_id = ? ORDER BY at DESC LIMIT 15')
      .all(s.user.id) as { event: string; detail: string | null; ip: string | null; at: number }[]
    return c.html(<DevicesPage user={s.user} csrf={s.csrf} devices={listDevices(db, s.user.id)} activity={activity} />)
  })

  app.post('/devices/revoke-all', requireSession, csrf, (c) => {
    revokeAllDevices(db, c.get('session')!.user.id, c.get('ip'))
    return c.redirect('/devices', 303)
  })

  app.post('/devices/:id/revoke', requireSession, csrf, (c) => {
    revokeDevice(db, c.get('session')!.user.id, c.req.param('id'), 'revoked_from_web', c.get('ip'))
    return c.redirect('/devices', 303)
  })

  app.post('/devices/:id/rename', requireSession, csrf, async (c) => {
    const body = await c.req.parseBody()
    renameDevice(db, c.get('session')!.user.id, c.req.param('id'), cleanLabel(body.name, 'GNOME device'))
    return c.redirect('/devices', 303)
  })

  return app
}

export type { Ctx }
