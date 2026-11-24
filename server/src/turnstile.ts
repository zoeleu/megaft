export interface TurnstileResult {
  success: boolean
  errors: string[]
}

export interface TurnstileVerifier {
  verify(token: string, remoteIp: string | null): Promise<TurnstileResult>
}

const SITEVERIFY = 'https://challenges.cloudflare.com/turnstile/v0/siteverify'

/** Server-side Turnstile validation. Tokens are single use and valid for 5 minutes (enforced by Cloudflare). */
export function cloudflareTurnstile(secret: string): TurnstileVerifier {
  return {
    async verify(token, remoteIp) {
      if (!token || token.length > 2048) return { success: false, errors: ['invalid-input-response'] }
      const body = new URLSearchParams({ secret, response: token, idempotency_key: crypto.randomUUID() })
      if (remoteIp) body.set('remoteip', remoteIp)
      try {
        const res = await fetch(SITEVERIFY, { method: 'POST', body, signal: AbortSignal.timeout(8000) })
        const json = (await res.json()) as { success?: boolean; 'error-codes'?: string[] }
        return { success: json.success === true, errors: json['error-codes'] ?? [] }
      } catch (err) {
        console.error('turnstile: siteverify failed', err)
        return { success: false, errors: ['internal-error'] }
      }
    },
  }
}
