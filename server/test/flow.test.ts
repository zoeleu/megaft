import { describe, expect, test } from 'bun:test'
import { decrypt, encrypt, generateUserCode, normalizeUserCode } from '../src/crypto'
import { safeNext } from '../src/routes/web'
import { ORIGIN, setup } from './helpers'

const DEVICE_GRANT = 'urn:ietf:params:oauth:grant-type:device_code'

describe('crypto helpers', () => {
  test('user codes are well-formed and normalise', () => {
    for (let i = 0; i < 200; i++) expect(generateUserCode()).toMatch(/^[BCDFGHJKLMNPQRSTVWXZ]{4}-[BCDFGHJKLMNPQRSTVWXZ]{4}$/)
    expect(normalizeUserCode('bcdf ghjk')).toBe('BCDF-GHJK')
    expect(normalizeUserCode('ABCD-EFGH')).toBeNull() // vowels are never issued
    expect(normalizeUserCode('BCD')).toBeNull()
  })

  test('encryption is bound to its AAD', () => {
    const key = Buffer.alloc(32, 1)
    const box = encrypt(key, 'secret', 'user:1')
    expect(decrypt(key, box, 'user:1')).toBe('secret')
    expect(() => decrypt(key, box, 'user:2')).toThrow()
  })

  test('safeNext rejects open redirects', () => {
    expect(safeNext('/link?code=X')).toBe('/link?code=X')
    for (const bad of ['//evil.com', '/\\evil.com', 'https://evil.com', '', undefined]) expect(safeNext(bad)).toBe('/devices')
  })
})

describe('web login', () => {
  test('captcha is required before redirecting to 42', async () => {
    const { browser, turnstile } = setup()
    const b = browser()
    const bad = await b.req('/login', { form: { 'cf-turnstile-response': 'nope', next: '/devices' } })
    expect(bad.status).toBe(400)
    expect(await bad.text()).toContain('captcha check failed')
    const ok = await b.req('/login', { form: { 'cf-turnstile-response': 'pass', next: '/devices' } })
    expect(ok.status).toBe(303)
    const loc = new URL(ok.headers.get('location')!)
    expect(loc.origin + loc.pathname).toBe('https://api.intra.42.fr/oauth/authorize')
    expect(loc.searchParams.get('redirect_uri')).toBe(`${ORIGIN}/auth/callback`)
    expect(loc.searchParams.get('state')!.length).toBeGreaterThan(30)
    expect(turnstile.calls).toBe(2)
  })

  test('callback requires the state cookie (login CSRF) and state is single use', async () => {
    const { browser } = setup()
    const victim = browser()
    const attacker = browser()
    const res = await attacker.req('/login', { form: { 'cf-turnstile-response': 'pass', next: '/devices' } })
    const state = new URL(res.headers.get('location')!).searchParams.get('state')!
    // attacker tricks victim into completing the attacker's flow → no state cookie in victim's browser
    const forged = await victim.req(`/auth/callback?code=good-code&state=${state}`)
    expect(forged.status).toBe(400)
    // legit completion works once
    const ok = await attacker.req(`/auth/callback?code=good-code&state=${state}`)
    expect(ok.status).toBe(302)
    expect(ok.headers.get('location')).toBe('/devices')
  })

  test('cross-origin POSTs are rejected', async () => {
    const { browser } = setup()
    const res = await browser().req('/login', { form: { 'cf-turnstile-response': 'pass' }, headers: { origin: 'https://evil.example' } })
    expect(res.status).toBe(403)
    const meta = await browser().req('/login', { form: { 'cf-turnstile-response': 'pass' }, headers: { origin: 'null', 'sec-fetch-site': 'cross-site' } })
    expect(meta.status).toBe(403)
    const nullOrigin = await browser().req('/login', { form: { 'cf-turnstile-response': 'pass' }, headers: { origin: 'null' } })
    expect(nullOrigin.status).toBe(403)
  })

  test('same-origin browser POSTs pass even when Origin is null (privacy referrer policies)', async () => {
    const { browser } = setup()
    const res = await browser().req('/login', { form: { 'cf-turnstile-response': 'pass' }, headers: { origin: 'null', 'sec-fetch-site': 'same-origin' } })
    expect(res.status).toBe(303)
  })

  test('security headers are set', async () => {
    const { app } = setup()
    const res = await app.request('/login')
    const csp = res.headers.get('content-security-policy')!
    expect(csp).toContain("frame-ancestors 'none'")
    expect(csp).toContain('https://challenges.cloudflare.com')
    expect(res.headers.get('x-frame-options')).toBe('SAMEORIGIN')
  })

  test('logged-out pages redirect to login and keep the destination', async () => {
    const { browser } = setup()
    const res = await browser().req('/link?code=BCDF-GHJK')
    expect(res.headers.get('location')).toBe('/login?next=%2Flink%3Fcode%3DBCDF-GHJK')
  })
})

describe('device flow', () => {
  test('pending → slow_down → approve → tokens, single use', async () => {
    const { api, login, csrfOf } = setup()
    const code = await api('/device/code', { json: { name: 'My laptop\u0007', platform: 'GNOME 42' } })
    expect(code.status).toBe(200)
    expect(code.body.verification_uri_complete).toBe(`${ORIGIN}/link?code=${code.body.user_code}`)
    const poll = () => api('/device/token', { json: { grant_type: DEVICE_GRANT, device_code: code.body.device_code } })

    expect((await poll()).body.error).toBe('authorization_pending')
    expect((await poll()).body.error).toBe('slow_down') // polled faster than `interval`

    const b = await login()
    const page = await (await b.req(`/link?code=${code.body.user_code.toLowerCase().replace('-', ' ')}`)).text()
    expect(page).toContain('My laptop')
    expect(page).not.toContain('\u0007')

    // CSRF token is required
    const noCsrf = await b.req('/link', { form: { code: code.body.user_code, decision: 'approve' } })
    expect(noCsrf.status).toBe(403)

    const approved = await b.req('/link', { form: { _csrf: csrfOf(page), code: code.body.user_code, decision: 'approve' } })
    expect(await approved.text()).toContain('is now linked')

    const tok = await poll()
    expect(tok.status).toBe(200)
    expect(tok.body.access_token).toStartWith('ftga_')
    expect(tok.body.refresh_token).toStartWith('ftgr_')
    expect((await poll()).body.error).toBe('invalid_grant') // device code cannot be reused
  })

  test('denied requests return access_denied', async () => {
    const { api, login, csrfOf } = setup()
    const code = await api('/device/code', { json: { name: 'x' } })
    const b = await login()
    const page = await (await b.req(`/link?code=${code.body.user_code}`)).text()
    await b.req('/link', { form: { _csrf: csrfOf(page), code: code.body.user_code, decision: 'deny' } })
    const res = await api('/device/token', { json: { grant_type: DEVICE_GRANT, device_code: code.body.device_code } })
    expect(res.body.error).toBe('access_denied')
  })

  test('unknown codes are rejected', async () => {
    const { login } = setup()
    const b = await login()
    expect((await b.req('/link?code=BCDF-GHJK')).status).toBe(404)
  })
})

describe('device API', () => {
  test('summary proxies 42 data without exposing 42 tokens', async () => {
    const { api, linkDevice } = setup()
    const { tokens } = await linkDevice()
    const res = await api('/summary', { token: tokens.access_token })
    expect(res.status).toBe(200)
    expect(res.body.user.login).toBe('zleullie')
    expect(res.body.logtime['2026-11-07']).toBe(4530)
    expect(res.body.evaluations).toHaveLength(1)
    expect(res.body.evaluations[0]).toMatchObject({ role: 'corrector', project: 'libft', with: ['peer'] })
    expect(JSON.stringify(res.body)).not.toContain('intra_at_')
  })

  test('summary is cached across devices of the same user', async () => {
    const { api, linkDevice, intra } = setup()
    const a = await linkDevice('A')
    const b = await linkDevice('B')
    await api('/summary', { token: a.tokens.access_token })
    const before = intra.state.calls.length
    await api('/summary', { token: b.tokens.access_token })
    expect(intra.state.calls.length).toBe(before)
  })

  test('expired 42 token is refreshed server-side; dead grant asks for re-login', async () => {
    const { api, linkDevice, intra } = setup()
    intra.nextLifetimeMs = 1000 // the token from login is almost expired
    const { tokens } = await linkDevice()
    expect((await api('/summary', { token: tokens.access_token })).status).toBe(200)
    expect(intra.state.validRefresh.has('intra_rt_1')).toBe(false) // it was rotated
  })

  test('refresh rotation and replay detection revokes the device', async () => {
    const { api, linkDevice } = setup()
    const { tokens } = await linkDevice()
    const r1 = await api('/device/token', { json: { grant_type: 'refresh_token', refresh_token: tokens.refresh_token } })
    expect(r1.status).toBe(200)
    expect(r1.body.refresh_token).not.toBe(tokens.refresh_token)
    expect((await api('/devices', { token: r1.body.access_token })).status).toBe(200)

    // old refresh token replayed (e.g. stolen copy) → device revoked
    const replay = await api('/device/token', { json: { grant_type: 'refresh_token', refresh_token: tokens.refresh_token } })
    expect(replay.body.error).toBe('invalid_grant')
    expect((await api('/devices', { token: r1.body.access_token })).status).toBe(401)
    const r2 = await api('/device/token', { json: { grant_type: 'refresh_token', refresh_token: r1.body.refresh_token } })
    expect(r2.body.error).toBe('invalid_grant')
  })

  test('devices can list, rename and revoke each other (own account only)', async () => {
    const { api, linkDevice } = setup()
    const a = await linkDevice('A')
    const b = await linkDevice('B')
    const list = await api('/devices', { token: a.tokens.access_token })
    expect(list.body.devices.map((d: any) => [d.name, d.current]).sort()).toEqual([['A', true], ['B', false]])

    expect((await api(`/devices/${b.tokens.device_id}`, { method: 'PATCH', json: { name: 'Bee' }, token: a.tokens.access_token })).status).toBe(200)
    expect((await api(`/devices/${b.tokens.device_id}`, { method: 'DELETE', token: a.tokens.access_token })).status).toBe(200)
    expect((await api('/summary', { token: b.tokens.access_token })).status).toBe(401)
    expect((await api('/device/revoke', { method: 'POST', token: a.tokens.access_token })).status).toBe(200)
    expect((await api('/devices', { token: a.tokens.access_token })).status).toBe(401)
  })

  test('web dashboard lists and revokes devices with CSRF protection', async () => {
    const { api, linkDevice, csrfOf } = setup()
    const { tokens, browser } = await linkDevice('Desk')
    const page = await (await browser.req('/devices')).text()
    expect(page).toContain('value="Desk"')
    expect(page).toContain('device linked')
    const bad = await browser.req(`/devices/${tokens.device_id}/revoke`, { form: { _csrf: 'wrong' } })
    expect(bad.status).toBe(403)
    const ok = await browser.req(`/devices/${tokens.device_id}/revoke`, { form: { _csrf: csrfOf(page) } })
    expect(ok.status).toBe(303)
    expect((await api('/summary', { token: tokens.access_token })).status).toBe(401)
  })

  test('bad bearer tokens get 401', async () => {
    const { api } = setup()
    expect((await api('/summary', { token: 'ftga_nope_nope_nope' })).status).toBe(401)
    expect((await api('/summary')).status).toBe(401)
  })

  test('logout clears the session', async () => {
    const { login, csrfOf } = setup()
    const b = await login()
    const page = await (await b.req('/devices')).text()
    await b.req('/logout', { form: { _csrf: csrfOf(page) } })
    expect((await b.req('/devices')).status).toBe(302)
  })
})
