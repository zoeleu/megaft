import { Hono, type MiddlewareHandler } from 'hono'
import { bodyLimit } from 'hono/body-limit'
import { rateLimit, type AppEnv, type Deps } from '../context'
import { IntraError } from '../intra'
import {
  authenticateAccessToken, cleanLabel, createDeviceRequest, DeviceFlowError, listDevices, pollDeviceCode, publicDevice,
  refreshDeviceTokens, renameDevice, revokeDevice, touchDevice,
} from '../services/devices'
import { getSummary } from '../services/summary'
import { ReauthRequired } from '../services/users'

const DEVICE_CODE_GRANT = 'urn:ietf:params:oauth:grant-type:device_code'

async function jsonBody(c: { req: { json: () => Promise<unknown> } }): Promise<Record<string, unknown>> {
  try {
    const b = await c.req.json()
    return b && typeof b === 'object' && !Array.isArray(b) ? (b as Record<string, unknown>) : {}
  } catch {
    return {}
  }
}

export function apiRoutes(deps: Deps) {
  const { db, config } = deps
  const api = new Hono<AppEnv>()

  api.use('*', bodyLimit({ maxSize: 8 * 1024, onError: (c) => c.json({ error: 'invalid_request', error_description: 'Body too large' }, 413) }))
  api.use('*', async (c, next) => {
    c.header('Cache-Control', 'no-store')
    await next()
  })

  // ── Device flow (client side) ────────────────────────────────────────────
  api.post('/device/code', rateLimit('device_code', 10, 10 * 60_000), async (c) => {
    const body = await jsonBody(c)
    const name = cleanLabel(body.name, 'GNOME device')
    const platform = cleanLabel(body.platform, '', 80) || null
    return c.json(createDeviceRequest(db, config, name, platform, c.get('ip')))
  })

  api.post('/device/token', rateLimit('device_token', 60, 60_000), async (c) => {
    const body = await jsonBody(c)
    try {
      if (body.grant_type === DEVICE_CODE_GRANT && typeof body.device_code === 'string')
        return c.json(pollDeviceCode(db, config, body.device_code, c.get('ip')))
      if (body.grant_type === 'refresh_token' && typeof body.refresh_token === 'string')
        return c.json(refreshDeviceTokens(db, config, body.refresh_token, c.get('ip')))
      return c.json({ error: 'invalid_request', error_description: 'Unsupported grant_type or missing parameter' }, 400)
    } catch (err) {
      if (err instanceof DeviceFlowError) return c.json({ error: err.code, error_description: err.message }, 400)
      throw err
    }
  })

  // ── Authenticated device API ─────────────────────────────────────────────
  const bearer: MiddlewareHandler<AppEnv> = async (c, next) => {
    const auth = c.req.header('authorization') ?? ''
    const m = /^Bearer ([A-Za-z0-9_\-]{10,100})$/.exec(auth)
    const dev = m ? authenticateAccessToken(db, m[1]!) : null
    if (!dev) {
      c.header('WWW-Authenticate', 'Bearer error="invalid_token"')
      return c.json({ error: 'invalid_token', error_description: 'Missing, expired or revoked access token' }, 401)
    }
    touchDevice(db, dev, c.get('ip'))
    c.set('device', dev)
    await next()
  }
  const perDevice = rateLimit('api', 120, 60_000, (c) => c.get('device')?.id ?? c.get('ip'))

  api.get('/summary', bearer, perDevice, async (c) => {
    try {
      return c.json(await getSummary(deps, c.get('device').user))
    } catch (err) {
      if (err instanceof ReauthRequired)
        return c.json({ error: 'intra_reauth_required', error_description: err.message, login_url: `${config.publicUrl}/login` }, 403)
      if (err instanceof IntraError) {
        console.error('summary: 42 API error', err.status, err.message)
        return c.json({ error: 'upstream_error', error_description: '42 Intra API is unavailable, retry later' }, 502)
      }
      throw err
    }
  })

  api.get('/devices', bearer, perDevice, (c) => {
    const dev = c.get('device')
    return c.json({ devices: listDevices(db, dev.user_id).map((d) => publicDevice(d, dev.id)) })
  })

  api.patch('/devices/:id', bearer, perDevice, async (c) => {
    const body = await jsonBody(c)
    const ok = renameDevice(db, c.get('device').user_id, c.req.param('id'), cleanLabel(body.name, 'GNOME device'))
    return ok ? c.json({ ok: true }) : c.json({ error: 'not_found' }, 404)
  })

  api.delete('/devices/:id', bearer, perDevice, (c) => {
    const dev = c.get('device')
    const ok = revokeDevice(db, dev.user_id, c.req.param('id'), `revoked_by_device:${dev.name}`, c.get('ip'))
    return ok ? c.json({ ok: true }) : c.json({ error: 'not_found' }, 404)
  })

  /** "Unlink this computer". */
  api.post('/device/revoke', bearer, (c) => {
    const dev = c.get('device')
    revokeDevice(db, dev.user_id, dev.id, 'unlinked_from_device', c.get('ip'))
    return c.json({ ok: true })
  })

  api.notFound((c) => c.json({ error: 'not_found' }, 404))
  return api
}
