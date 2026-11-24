import { createApp } from '../src/app'
import { loadConfig } from '../src/config'
import type { Deps } from '../src/context'
import { openDatabase } from '../src/db'
import { clearSummaryCache } from '../src/services/summary'
import { IntraError, type IntraClient, type IntraTokens } from '../src/intra'
import type { TurnstileVerifier } from '../src/turnstile'

export const ORIGIN = 'http://localhost:3000'

/** In-memory stand-in for the 42 Intra API. */
export function fakeIntra() {
  let n = 0
  const state = {
    validAccess: new Set<string>(),
    validRefresh: new Set<string>(),
    calls: [] as string[],
    failRefresh: false,
  }
  const mint = (lifetimeMs = 7200_000): IntraTokens => {
    n++
    const t = { access_token: `intra_at_${n}`, refresh_token: `intra_rt_${n}`, expires_at: Date.now() + lifetimeMs }
    state.validAccess.add(t.access_token)
    state.validRefresh.add(t.refresh_token)
    return t
  }
  const client: IntraClient & { state: typeof state; nextLifetimeMs: number } = {
    state,
    nextLifetimeMs: 7200_000,
    async exchangeCode(code) {
      if (code !== 'good-code') throw new IntraError(400, 'invalid_grant')
      return mint(client.nextLifetimeMs)
    },
    async refresh(rt) {
      if (state.failRefresh || !state.validRefresh.delete(rt)) throw new IntraError(400, 'invalid_grant')
      return mint()
    },
    async get<T>(path: string, token: string): Promise<T> {
      state.calls.push(path)
      if (!state.validAccess.has(token)) throw new IntraError(401, 'unauthorized')
      if (path === '/v2/me')
        return { id: 4242, login: 'zleullie', displayname: 'Zoe L', wallet: 10, correction_point: 5, location: 'c1r2p3' } as T
      if (path.startsWith('/v2/users/4242/locations_stats')) return { '2026-11-06': '03:30:00.000000', '2026-11-07': '01:15:30.5' } as T
      if (path.includes('as_corrector'))
        return [{ id: 1, begin_at: new Date(Date.now() + 3600_000).toISOString(), correcteds: [{ login: 'peer' }], team: { project_gitlab_path: 'pedago/libft' } }] as T
      if (path.includes('as_corrected')) return [{ id: 2, begin_at: new Date(Date.now() - 86400_000).toISOString(), corrector: 'invisible' }] as T
      throw new IntraError(404, 'not found')
    },
  }
  return client
}

export function fakeTurnstile(): TurnstileVerifier & { calls: number } {
  const t = {
    calls: 0,
    async verify(token: string) {
      t.calls++
      return token === 'pass' ? { success: true, errors: [] } : { success: false, errors: ['invalid-input-response'] }
    },
  }
  return t
}

export function setup() {
  clearSummaryCache()
  const config = loadConfig({
    PUBLIC_URL: ORIGIN,
    FT_CLIENT_UID: 'uid',
    FT_CLIENT_SECRET: 'secret',
    TURNSTILE_SITE_KEY: 'site',
    TURNSTILE_SITE_SECRET: 'tsecret',
    ENCRYPTION_KEY: Buffer.alloc(32, 7).toString('base64'),
  })
  const intra = fakeIntra()
  const turnstile = fakeTurnstile()
  const deps: Deps = { db: openDatabase(':memory:'), config, intra, turnstile }
  const app = createApp(deps)

  /** Minimal browser: keeps cookies, sends same-origin headers on POST. */
  function browser() {
    const jar = new Map<string, string>()
    async function req(path: string, init: { method?: string; form?: Record<string, string>; headers?: Record<string, string> } = {}) {
      const headers: Record<string, string> = { ...init.headers }
      if (jar.size) headers.cookie = [...jar].map(([k, v]) => `${k}=${v}`).join('; ')
      let body: string | undefined
      if (init.form) {
        body = new URLSearchParams(init.form).toString()
        headers['content-type'] = 'application/x-www-form-urlencoded'
        headers.origin ??= ORIGIN
      }
      const res = await app.request(path, { method: init.method ?? (init.form ? 'POST' : 'GET'), headers, body })
      for (const sc of res.headers.getSetCookie()) {
        const [pair] = sc.split(';')
        const [k, v] = pair!.split('=')
        if (/max-age=0/i.test(sc) || v === '') jar.delete(k!)
        else jar.set(k!, v!)
      }
      return res
    }
    return { req, jar }
  }

  async function api(path: string, init: { method?: string; json?: unknown; token?: string } = {}) {
    const headers: Record<string, string> = {}
    if (init.json !== undefined) headers['content-type'] = 'application/json'
    if (init.token) headers.authorization = `Bearer ${init.token}`
    const res = await app.request(`/api/v1${path}`, {
      method: init.method ?? (init.json !== undefined ? 'POST' : 'GET'),
      headers,
      body: init.json !== undefined ? JSON.stringify(init.json) : undefined,
    })
    return { status: res.status, body: (await res.json()) as any }
  }

  /** Full browser login via Turnstile + (fake) 42 OAuth. Returns the logged-in browser. */
  async function login(next = '/devices') {
    const b = browser()
    const res = await b.req('/login', { form: { 'cf-turnstile-response': 'pass', next } })
    const loc = new URL(res.headers.get('location')!)
    const state = loc.searchParams.get('state')!
    const cb = await b.req(`/auth/callback?code=good-code&state=${encodeURIComponent(state)}`)
    return { ...b, landing: cb }
  }

  function csrfOf(html: string) {
    return /name="_csrf" value="([^"]+)"/.exec(html)![1]!
  }

  /** Runs the whole device flow and returns the issued token pair. */
  async function linkDevice(name = 'Laptop') {
    const code = await api('/device/code', { json: { name, platform: 'GNOME 42' } })
    const b = await login()
    const page = await (await b.req(`/link?code=${code.body.user_code}`)).text()
    await b.req('/link', { form: { _csrf: csrfOf(page), code: code.body.user_code, decision: 'approve' } })
    const tok = await api('/device/token', { json: { grant_type: 'urn:ietf:params:oauth:grant-type:device_code', device_code: code.body.device_code } })
    return { tokens: tok.body, browser: b }
  }

  return { deps, app, intra, turnstile, browser, api, login, csrfOf, linkDevice }
}
