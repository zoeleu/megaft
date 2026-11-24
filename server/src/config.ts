export type TrustProxy = 'none' | 'cloudflare' | 'x-forwarded-for'

export interface Config {
  /** Public origin of this server, e.g. https://ftgnome.example.com (no trailing slash). */
  publicUrl: string
  port: number
  databasePath: string
  /** 42 OAuth application credentials. */
  ftClientId: string
  ftClientSecret: string
  /** Cloudflare Turnstile keys. */
  turnstileSiteKey: string
  turnstileSecret: string
  /** 32-byte key used to encrypt 42 tokens at rest (AES-256-GCM). */
  encryptionKey: Buffer
  /** Which header (if any) to trust for the client IP. */
  trustProxy: TrustProxy
  /** Send cookies with the Secure flag (true unless PUBLIC_URL is plain http). */
  secureCookies: boolean

  sessionTtlMs: number
  deviceCodeTtlMs: number
  devicePollIntervalS: number
  accessTokenTtlMs: number
  refreshTokenTtlMs: number
}

function required(env: Record<string, string | undefined>, name: string): string {
  const v = env[name]
  if (!v || !v.trim()) throw new Error(`Missing required environment variable ${name}`)
  return v.trim()
}

export function loadConfig(env: Record<string, string | undefined> = process.env): Config {
  const publicUrl = required(env, 'PUBLIC_URL').replace(/\/+$/, '')
  const url = new URL(publicUrl)
  const isLocal = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
  if (url.protocol !== 'https:' && !isLocal)
    throw new Error('PUBLIC_URL must use https:// (plain http is only allowed for localhost)')

  const key = Buffer.from(required(env, 'ENCRYPTION_KEY'), 'base64')
  if (key.length !== 32)
    throw new Error('ENCRYPTION_KEY must be 32 random bytes, base64-encoded (run `bun run gen-key`)')

  const trustProxy = (env.TRUST_PROXY ?? 'none') as TrustProxy
  if (!['none', 'cloudflare', 'x-forwarded-for'].includes(trustProxy))
    throw new Error('TRUST_PROXY must be one of: none, cloudflare, x-forwarded-for')

  return {
    publicUrl,
    port: Number(env.PORT ?? 3000),
    databasePath: env.DATABASE_PATH ?? 'data/ftgnome.sqlite',
    ftClientId: required(env, 'FT_CLIENT_UID'),
    ftClientSecret: required(env, 'FT_CLIENT_SECRET'),
    turnstileSiteKey: required(env, 'TURNSTILE_SITE_KEY'),
    turnstileSecret: required(env, 'TURNSTILE_SITE_SECRET'),
    encryptionKey: key,
    trustProxy,
    secureCookies: url.protocol === 'https:',

    sessionTtlMs: 7 * 24 * 3600_000,
    deviceCodeTtlMs: 10 * 60_000,
    devicePollIntervalS: 5,
    accessTokenTtlMs: 3600_000,
    refreshTokenTtlMs: 60 * 24 * 3600_000,
  }
}

export function redirectUri(cfg: Config): string {
  return `${cfg.publicUrl}/auth/callback`
}
