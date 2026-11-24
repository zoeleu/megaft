import { createCipheriv, createDecipheriv, createHash, randomBytes, timingSafeEqual } from 'node:crypto'

/** URL-safe random token, optionally prefixed so leaked tokens are easy to identify (e.g. by secret scanners). */
export function randomToken(bytes = 32, prefix = ''): string {
  return prefix + randomBytes(bytes).toString('base64url')
}

/** Tokens are stored only as SHA-256 hashes: a database leak does not leak usable credentials. */
export function hashToken(token: string): string {
  return createHash('sha256').update(token, 'utf8').digest('hex')
}

export function safeEqual(a: string, b: string): boolean {
  const ab = Buffer.from(a)
  const bb = Buffer.from(b)
  return ab.length === bb.length && timingSafeEqual(ab, bb)
}

// RFC 8628 §6.1: consonant-only alphabet, no vowels (no accidental words), no ambiguous chars.
const USER_CODE_ALPHABET = 'BCDFGHJKLMNPQRSTVWXZ'

/** 8-character user code shown as XXXX-XXXX (20^8 ≈ 2.6e10 combinations). */
export function generateUserCode(): string {
  const out: string[] = []
  while (out.length < 8) {
    for (const b of randomBytes(16)) {
      // rejection sampling: 240 is the largest multiple of 20 below 256, so no modulo bias
      if (b < 240 && out.length < 8) out.push(USER_CODE_ALPHABET[b % 20]!)
    }
  }
  return `${out.slice(0, 4).join('')}-${out.slice(4).join('')}`
}

/** Normalise user input ("bcdf ghjk", "BCDFGHJK", "bcdf-ghjk") to the canonical XXXX-XXXX form, or null. */
export function normalizeUserCode(input: string | undefined | null): string | null {
  if (!input) return null
  const letters = input.toUpperCase().replace(/[^A-Z]/g, '')
  if (letters.length !== 8) return null
  for (const ch of letters) if (!USER_CODE_ALPHABET.includes(ch)) return null
  return `${letters.slice(0, 4)}-${letters.slice(4)}`
}

/** AES-256-GCM. `aad` binds the ciphertext to its owner so rows cannot be swapped between users. */
export function encrypt(key: Buffer, plaintext: string, aad: string): string {
  const iv = randomBytes(12)
  const cipher = createCipheriv('aes-256-gcm', key, iv)
  cipher.setAAD(Buffer.from(aad, 'utf8'))
  const data = Buffer.concat([cipher.update(plaintext, 'utf8'), cipher.final()])
  return ['v1', iv.toString('base64'), cipher.getAuthTag().toString('base64'), data.toString('base64')].join(':')
}

export function decrypt(key: Buffer, payload: string, aad: string): string {
  const [version, iv, tag, data] = payload.split(':')
  if (version !== 'v1' || !iv || !tag || !data) throw new Error('Invalid encrypted payload')
  const decipher = createDecipheriv('aes-256-gcm', key, Buffer.from(iv, 'base64'))
  decipher.setAAD(Buffer.from(aad, 'utf8'))
  decipher.setAuthTag(Buffer.from(tag, 'base64'))
  return Buffer.concat([decipher.update(Buffer.from(data, 'base64')), decipher.final()]).toString('utf8')
}
