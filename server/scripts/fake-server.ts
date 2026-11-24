/**
 * Dev/test server with a fake 42 API and an always-pass captcha, so the GNOME
 * extension can be exercised end to end without real credentials.
 * Adds POST /__test/approve {user_code}. NEVER deploy this file.
 */
import { createApp } from '../src/app'
import { loadConfig } from '../src/config'
import { openDatabase } from '../src/db'
import { decideDeviceRequest } from '../src/services/devices'
import { upsertUser } from '../src/services/users'
import { fakeIntra, fakeTurnstile } from '../test/helpers'

const port = Number(process.env.PORT ?? 3999)
const config = loadConfig({
  PUBLIC_URL: `http://localhost:${port}`, PORT: String(port),
  FT_CLIENT_UID: 'uid', FT_CLIENT_SECRET: 'secret', TURNSTILE_SITE_KEY: '1x00000000000000000000AA', TURNSTILE_SITE_SECRET: 'x',
  ENCRYPTION_KEY: Buffer.alloc(32, 9).toString('base64'),
})
const intra = fakeIntra()
const deps = { db: openDatabase(':memory:'), config, intra, turnstile: fakeTurnstile() }
const tokens = await intra.exchangeCode('good-code', '')
const user = upsertUser(deps, await intra.get('/v2/me', tokens.access_token), tokens)

const app = createApp(deps)
const outer = new (await import('hono')).Hono()
outer.post('/__test/approve', async (c) => {
  const { user_code } = await c.req.json()
  return c.json({ ok: !!decideDeviceRequest(deps.db, user_code, user.id, true, null) })
})
outer.route('/', app)
console.log(`fake server on http://localhost:${port}`)
export default { port, fetch: outer.fetch }
