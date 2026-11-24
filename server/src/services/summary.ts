import type { Deps } from '../context'
import type { UserRow } from '../db'
import { IntraError, type IntraMe } from '../intra'
import { intraAccessToken } from './users'

/** Everything the panel indicator needs, in one call. Cached per user so many devices ≠ many 42 calls. */
export interface Summary {
  user: { login: string; display_name: string | null; image_url: string | null; wallet: number | null; correction_points: number | null; location: string | null }
  /** seconds logged per day, keyed by YYYY-MM-DD (as reported by 42), last 8 days */
  logtime: Record<string, number>
  evaluations: Evaluation[]
  fetched_at: string
}

export interface Evaluation {
  id: number
  begin_at: string
  role: 'corrector' | 'corrected'
  project: string | null
  /** the other side's logins; null while 42 still hides them */
  with: string[] | null
}

const TTL_MS = 60_000
const cache = new Map<number, { at: number; data: Summary }>()
const inflight = new Map<number, Promise<Summary>>()

export function clearSummaryCache(userId?: number) {
  if (userId === undefined) cache.clear()
  else cache.delete(userId)
}

export async function getSummary(deps: Deps, user: UserRow): Promise<Summary> {
  const hit = cache.get(user.id)
  if (hit && Date.now() - hit.at < TTL_MS) return hit.data
  let p = inflight.get(user.id)
  if (!p) {
    p = buildSummary(deps, user)
      .then((data) => {
        cache.set(user.id, { at: Date.now(), data })
        return data
      })
      .finally(() => inflight.delete(user.id))
    inflight.set(user.id, p)
  }
  return p
}

async function buildSummary(deps: Deps, user: UserRow): Promise<Summary> {
  const token = await intraAccessToken(deps, user.id)
  const me = await deps.intra.get<IntraMe>('/v2/me', token)

  const since = new Date(Date.now() - 8 * 86400_000).toISOString().slice(0, 10)
  const stats = await deps.intra.get<Record<string, string>>(
    `/v2/users/${me.id}/locations_stats?begin_at=${since}`, token)

  const [asCorrector, asCorrected] = [
    await scaleTeams(deps, token, 'as_corrector'),
    await scaleTeams(deps, token, 'as_corrected'),
  ]
  const now = Date.now()
  const evaluations = [
    ...asCorrector.map((t) => toEvaluation(t, 'corrector')),
    ...asCorrected.map((t) => toEvaluation(t, 'corrected')),
  ]
    .filter((e): e is Evaluation => e !== null && Date.parse(e.begin_at) > now - 15 * 60_000)
    .sort((a, b) => Date.parse(a.begin_at) - Date.parse(b.begin_at))
    .slice(0, 10)

  return {
    user: {
      login: me.login,
      display_name: me.displayname ?? null,
      image_url: me.image?.versions?.small ?? me.image?.link ?? null,
      wallet: me.wallet ?? null,
      correction_points: me.correction_point ?? null,
      location: me.location ?? null,
    },
    logtime: Object.fromEntries(Object.entries(stats).map(([day, v]) => [day, parseDuration(v)])),
    evaluations,
    fetched_at: new Date().toISOString(),
  }
}

/** "05:12:33.123456" → seconds */
export function parseDuration(v: string): number {
  const m = /^(\d+):(\d{2}):(\d{2})(?:\.\d+)?$/.exec(v)
  return m ? Number(m[1]) * 3600 + Number(m[2]) * 60 + Number(m[3]) : 0
}

type ScaleTeam = {
  id: number
  begin_at: string | null
  filled_at?: string | null
  corrector?: { login?: string } | string | null
  correcteds?: Array<{ login?: string }> | string | null
  team?: { name?: string; project_gitlab_path?: string | null } | null
}

async function scaleTeams(deps: Deps, token: string, role: 'as_corrector' | 'as_corrected'): Promise<ScaleTeam[]> {
  const base = `/v2/me/scale_teams/${role}?page[size]=50&sort=begin_at`
  try {
    return await deps.intra.get<ScaleTeam[]>(`${base}&filter[future]=true`, token)
  } catch (err) {
    // some API versions reject the filter; fall back and filter locally
    if (err instanceof IntraError && (err.status === 400 || err.status === 422))
      return deps.intra.get<ScaleTeam[]>(base, token)
    throw err
  }
}

function toEvaluation(t: ScaleTeam, role: Evaluation['role']): Evaluation | null {
  if (!t.begin_at || t.filled_at) return null
  let withLogins: string[] | null = null
  if (role === 'corrector' && Array.isArray(t.correcteds))
    withLogins = t.correcteds.map((c) => c.login).filter((l): l is string => !!l)
  if (role === 'corrected' && t.corrector && typeof t.corrector === 'object' && t.corrector.login)
    withLogins = [t.corrector.login]
  const gitlab = t.team?.project_gitlab_path?.split('/').pop()
  return { id: t.id, begin_at: t.begin_at, role, project: gitlab || t.team?.name || null, with: withLogins }
}
