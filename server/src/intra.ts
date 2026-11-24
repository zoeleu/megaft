/** Minimal 42 Intra API client. All calls go through a global pacer (42 allows 2 req/s per application). */

export const INTRA_API = 'https://api.intra.42.fr'
export const INTRA_AUTHORIZE = 'https://api.intra.42.fr/oauth/authorize'

export interface IntraTokens {
  access_token: string
  refresh_token: string | null
  /** absolute expiry, ms since epoch */
  expires_at: number
}

export interface IntraMe {
  id: number
  login: string
  displayname?: string
  image?: { link?: string | null; versions?: { small?: string | null } }
  wallet?: number
  correction_point?: number
  location?: string | null
}

export class IntraError extends Error {
  constructor(public status: number, message: string) {
    super(message)
  }
}

export interface IntraClient {
  exchangeCode(code: string, redirectUri: string): Promise<IntraTokens>
  refresh(refreshToken: string): Promise<IntraTokens>
  get<T>(path: string, accessToken: string): Promise<T>
}

function createPacer(minGapMs: number) {
  let next = 0
  return async function pace() {
    const now = Date.now()
    const at = Math.max(now, next)
    next = at + minGapMs
    if (at > now) await Bun.sleep(at - now)
  }
}

export function httpIntraClient(clientId: string, clientSecret: string): IntraClient {
  const pace = createPacer(550)

  async function tokenRequest(params: Record<string, string>): Promise<IntraTokens> {
    await pace()
    // credentials go in the POST body, never in the URL (URLs end up in logs)
    const res = await fetch(`${INTRA_API}/oauth/token`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded', Accept: 'application/json' },
      body: new URLSearchParams({ ...params, client_id: clientId, client_secret: clientSecret }),
      signal: AbortSignal.timeout(10_000),
    })
    const json = (await res.json().catch(() => ({}))) as Record<string, unknown>
    if (!res.ok || typeof json.access_token !== 'string')
      throw new IntraError(res.status, `token endpoint: ${String(json.error ?? res.status)}`)
    const expiresIn = typeof json.expires_in === 'number' ? json.expires_in : 7200
    return {
      access_token: json.access_token,
      refresh_token: typeof json.refresh_token === 'string' ? json.refresh_token : null,
      expires_at: Date.now() + expiresIn * 1000,
    }
  }

  return {
    exchangeCode: (code, redirectUri) =>
      tokenRequest({ grant_type: 'authorization_code', code, redirect_uri: redirectUri }),
    refresh: (refreshToken) => tokenRequest({ grant_type: 'refresh_token', refresh_token: refreshToken }),
    async get<T>(path: string, accessToken: string): Promise<T> {
      await pace()
      const res = await fetch(`${INTRA_API}${path}`, {
        headers: { Authorization: `Bearer ${accessToken}`, Accept: 'application/json' },
        signal: AbortSignal.timeout(10_000),
      })
      if (!res.ok) throw new IntraError(res.status, `GET ${path.split('?')[0]}: HTTP ${res.status}`)
      return (await res.json()) as T
    },
  }
}
