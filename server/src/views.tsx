import type { Child } from 'hono/jsx'
import type { DeviceRequestRow, DeviceRow, UserRow } from './db'

export const CSS = `
:root{--bg:#f6f6f8;--fg:#1d1d22;--muted:#6b6b76;--card:#fff;--line:#e3e3e8;--accent:#00babc;--danger:#c0392b;color-scheme:light dark}
@media (prefers-color-scheme:dark){:root{--bg:#16161a;--fg:#ececf1;--muted:#9a9aa6;--card:#1f1f25;--line:#2e2e36}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,-apple-system,"Cantarell",sans-serif}
header{display:flex;align-items:center;gap:16px;padding:12px 20px;border-bottom:1px solid var(--line);background:var(--card)}
header .brand{font-weight:700;margin-right:auto}header a{color:inherit;text-decoration:none}
main{max-width:760px;margin:32px auto;padding:0 16px}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:20px 22px;margin-bottom:18px}
h1{font-size:22px;margin:0 0 6px}h2{font-size:17px;margin:0 0 10px}p{margin:6px 0}.muted{color:var(--muted)}
.code{font:600 28px/1.2 ui-monospace,monospace;letter-spacing:4px;padding:10px 14px;border:1px dashed var(--line);border-radius:8px;display:inline-block}
button,.btn{font:inherit;border:1px solid var(--line);background:var(--card);color:var(--fg);padding:7px 14px;border-radius:8px;cursor:pointer}
button.primary{background:var(--accent);border-color:var(--accent);color:#fff}button.danger{color:var(--danger);border-color:var(--danger)}
input[type=text]{font:inherit;padding:7px 10px;border:1px solid var(--line);border-radius:8px;background:var(--bg);color:var(--fg)}
.row{display:flex;gap:10px;align-items:center;flex-wrap:wrap}
table{width:100%;border-collapse:collapse}td,th{text-align:left;padding:10px 6px;border-top:1px solid var(--line);vertical-align:top}th{font-size:13px;color:var(--muted);border-top:0}
.warn{border-left:4px solid #e0a800;padding:8px 12px;background:rgba(224,168,0,.08);border-radius:6px}
.err{border-left:4px solid var(--danger);padding:8px 12px;background:rgba(192,57,43,.08);border-radius:6px}
.tag{font-size:12px;padding:1px 8px;border-radius:99px;background:var(--accent);color:#fff}
form.inline{display:inline}.mt{margin-top:14px}.my{margin:14px 0}.grow{margin-right:auto}
@media (max-width:560px){.hide-sm{display:none}}
`

const fmt = (ms: number | null) => (ms ? new Date(ms).toISOString().replace('T', ' ').slice(0, 16) + ' UTC' : '—')

export function Layout(props: { title: string; user?: UserRow | null; csrf?: string; children: Child; turnstile?: boolean }) {
  return (
    <html lang="en">
      <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <meta name="referrer" content="same-origin" />
        <title>{props.title} · ft_gnome</title>
        <link rel="stylesheet" href="/assets/app.css" />
        {props.turnstile && <script src="https://challenges.cloudflare.com/turnstile/v0/api.js" async defer></script>}
      </head>
      <body>
        <header>
          <a class="brand" href="/">ft_gnome</a>
          {props.user && (
            <>
              <a href="/devices">Devices</a>
              <a href="/link">Link a device</a>
              <form class="inline" method="post" action="/logout">
                <input type="hidden" name="_csrf" value={props.csrf} />
                <button type="submit">Sign out ({props.user.login})</button>
              </form>
            </>
          )}
        </header>
        <main>{props.children}</main>
      </body>
    </html>
  )
}

export function LoginPage(p: { siteKey: string; next: string; error?: string }) {
  return (
    <Layout title="Sign in" turnstile>
      <div class="card">
        <h1>Sign in with 42</h1>
        <p class="muted">Complete the check, then you'll be sent to the 42 Intra to authorise ft_gnome.</p>
        {p.error && <p class="err">{p.error}</p>}
        <form method="post" action="/login">
          <input type="hidden" name="next" value={p.next} />
          <div class="cf-turnstile my" data-sitekey={p.siteKey} data-action="login"></div>
          <button class="primary" type="submit">Continue to 42</button>
        </form>
      </div>
    </Layout>
  )
}

export function EnterCodePage(p: { user: UserRow; csrf: string; error?: string }) {
  return (
    <Layout title="Link a device" user={p.user} csrf={p.csrf}>
      <div class="card">
        <h1>Link a device</h1>
        <p class="muted">Enter the code shown by the ft_gnome extension on your computer.</p>
        {p.error && <p class="err">{p.error}</p>}
        <form method="get" action="/link" class="row mt">
          <input type="text" name="code" placeholder="XXXX-XXXX" autocomplete="off" autocapitalize="characters" maxlength={9} required />
          <button class="primary" type="submit">Next</button>
        </form>
      </div>
    </Layout>
  )
}

export function ConfirmLinkPage(p: { user: UserRow; csrf: string; req: DeviceRequestRow; currentIp: string | null }) {
  const sameIp = p.req.ip && p.currentIp && p.req.ip === p.currentIp
  return (
    <Layout title="Approve device" user={p.user} csrf={p.csrf}>
      <div class="card">
        <h1>Approve this device?</h1>
        <p>
          <span class="code">{p.req.user_code}</span>
        </p>
        <table class="my">
          <tr><th>Device name</th><td>{p.req.name}</td></tr>
          <tr><th>Platform</th><td>{p.req.platform ?? '—'}</td></tr>
          <tr><th>Requested</th><td>{fmt(p.req.created_at)}</td></tr>
          <tr><th>Requested from</th><td>{p.req.ip ?? 'unknown'} {sameIp ? <span class="tag">same network as you</span> : null}</td></tr>
        </table>
        <p class="warn">
          Only approve if <b>you</b> just started linking on your own computer and the code matches what it shows.
          If someone sent you this link, deny it: approving gives them access to your 42 data.
        </p>
        <div class="row mt">
          <form method="post" action="/link">
            <input type="hidden" name="_csrf" value={p.csrf} />
            <input type="hidden" name="code" value={p.req.user_code} />
            <input type="hidden" name="decision" value="approve" />
            <button class="primary" type="submit">Approve as {p.user.login}</button>
          </form>
          <form method="post" action="/link">
            <input type="hidden" name="_csrf" value={p.csrf} />
            <input type="hidden" name="code" value={p.req.user_code} />
            <input type="hidden" name="decision" value="deny" />
            <button class="danger" type="submit">Deny</button>
          </form>
        </div>
      </div>
    </Layout>
  )
}

export function MessagePage(p: { title: string; message: string; user?: UserRow | null; csrf?: string; error?: boolean }) {
  return (
    <Layout title={p.title} user={p.user} csrf={p.csrf}>
      <div class="card">
        <h1>{p.title}</h1>
        <p class={p.error ? 'err' : ''}>{p.message}</p>
        <p class="mt"><a class="btn" href="/devices">Go to your devices</a></p>
      </div>
    </Layout>
  )
}

export function DevicesPage(p: {
  user: UserRow
  csrf: string
  devices: DeviceRow[]
  activity: { event: string; detail: string | null; ip: string | null; at: number }[]
}) {
  return (
    <Layout title="Devices" user={p.user} csrf={p.csrf}>
      <div class="card">
        <div class="row">
          <h1 class="grow">Your devices</h1>
          <a class="btn" href="/link">Link a device</a>
        </div>
        {p.devices.length === 0 ? (
          <p class="muted">No linked devices yet. Open the ft_gnome extension menu and choose “Link this computer”.</p>
        ) : (
          <table>
            <tr><th>Name</th><th class="hide-sm">Linked</th><th>Last seen</th><th></th></tr>
            {p.devices.map((d) => (
              <tr>
                <td>
                  <form method="post" action={`/devices/${d.id}/rename`} class="row">
                    <input type="hidden" name="_csrf" value={p.csrf} />
                    <input type="text" name="name" value={d.name} maxlength={64} aria-label="Device name" />
                    <button type="submit">Rename</button>
                  </form>
                  <span class="muted">{d.platform ?? ''}</span>
                </td>
                <td class="hide-sm">{fmt(d.created_at)}</td>
                <td>{fmt(d.last_seen_at)}<br /><span class="muted">{d.last_ip ?? ''}</span></td>
                <td>
                  <form method="post" action={`/devices/${d.id}/revoke`}>
                    <input type="hidden" name="_csrf" value={p.csrf} />
                    <button class="danger" type="submit">Revoke</button>
                  </form>
                </td>
              </tr>
            ))}
          </table>
        )}
        {p.devices.length > 1 && (
          <form method="post" action="/devices/revoke-all" class="mt">
            <input type="hidden" name="_csrf" value={p.csrf} />
            <button class="danger" type="submit">Revoke all devices</button>
          </form>
        )}
      </div>
      <div class="card">
        <h2>Recent security activity</h2>
        {p.activity.length === 0 ? <p class="muted">Nothing yet.</p> : (
          <table>
            {p.activity.map((a) => (
              <tr><td>{fmt(a.at)}</td><td>{a.event.replaceAll('_', ' ')}</td><td class="muted">{a.detail ?? ''}</td><td class="muted hide-sm">{a.ip ?? ''}</td></tr>
            ))}
          </table>
        )}
      </div>
    </Layout>
  )
}
