import { createApp } from './app'
import { loadConfig } from './config'
import { openDatabase, purgeExpired } from './db'
import { httpIntraClient } from './intra'
import { cloudflareTurnstile } from './turnstile'

const config = loadConfig()
const db = openDatabase(config.databasePath)
const app = createApp({
  db,
  config,
  intra: httpIntraClient(config.ftClientId, config.ftClientSecret),
  turnstile: cloudflareTurnstile(config.turnstileSecret),
})

purgeExpired(db)
setInterval(() => purgeExpired(db), 15 * 60_000).unref()

if (config.turnstileSiteKey.startsWith('1x0000') && process.env.NODE_ENV === 'production')
  console.warn('WARNING: Turnstile test keys in production — the captcha always passes')

console.log(`ft_gnome server on :${config.port} (public URL ${config.publicUrl})`)

export default { port: config.port, fetch: app.fetch }
