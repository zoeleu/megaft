import { Hono } from 'hono'
import { secureHeaders } from 'hono/secure-headers'
import { clientIpMiddleware, type AppEnv, type Deps } from './context'
import { apiRoutes } from './routes/api'
import { webRoutes } from './routes/web'

const TURNSTILE = 'https://challenges.cloudflare.com'

export function createApp(deps: Deps) {
  const app = new Hono<AppEnv>()

  app.use('*', clientIpMiddleware(deps))
  app.use(
    '*',
    secureHeaders({
      contentSecurityPolicy: {
        defaultSrc: ["'none'"],
        scriptSrc: ["'self'", TURNSTILE],
        frameSrc: [TURNSTILE],
        connectSrc: ["'self'", TURNSTILE],
        styleSrc: ["'self'"],
        imgSrc: ["'self'", 'data:'],
        // form-action also governs the redirect that follows POST /login
        formAction: ["'self'", 'https://api.intra.42.fr'],
        baseUri: ["'none'"],
        frameAncestors: ["'none'"],
      },
      strictTransportSecurity: deps.config.secureCookies ? 'max-age=31536000; includeSubDomains' : false,
      referrerPolicy: 'same-origin',
      crossOriginEmbedderPolicy: false,
    }),
  )

  app.get('/healthz', (c) => c.json({ ok: true }))
  app.route('/api/v1', apiRoutes(deps))
  app.route('/', webRoutes(deps))

  app.onError((err, c) => {
    console.error('unhandled error:', err)
    return c.req.path.startsWith('/api/')
      ? c.json({ error: 'server_error' }, 500)
      : c.text('Internal server error', 500)
  })

  return app
}
