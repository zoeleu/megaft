#!/usr/bin/env python3
"""Poll the 42 intra for an evaluation slot on a project and book the first one in a time window.

Uses the intra web client's own endpoints (the public API v2 can't book as a student):
  GET  /projects/<slug>/slots?team_id=..            -> HTML with team_id, duration, update-url, CSRF token
  GET  /projects/<slug>/slots.json?team_id=..&start=..&end=..   -> available blocks
  POST /projects/<slug>/slots/<ids>.json?team_id=.. (_method=put, start, end) -> book

Auth: cookies42.txt (Netscape format). They're fetched and refreshed automatically with
undetected-chromedriver (see browser_cookies.py) whenever they're missing, expired, or Cloudflare
challenges us. First time, run `.venv/bin/python browser_cookies.py --login` and log in once.

Example:
  .venv/bin/python evalfinder.py codexion --before 02:00            # book first slot starting before 2am (local)
  .venv/bin/python evalfinder.py codexion --hours 4 --count 2       # two evaluations within 4h
  .venv/bin/python evalfinder.py codexion --before 02:00 --dry-run  # just show what it would book
"""
import argparse, datetime as dt, html as htmllib, http.cookiejar, json, os, random, re, sys, threading, time
import urllib.error, urllib.parse, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = "https://projects.intra.42.fr"
# Must match the browser the cookies came from (Cloudflare's cf_clearance is bound to it).
DEFAULT_UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36"
SLOT_MIN = 15


LOG_HOOK = None  # set by the GUI to capture output


def log(*a):
    if LOG_HOOK:
        LOG_HOOK(" ".join(map(str, a)))
    else:
        print(dt.datetime.now().strftime("%H:%M:%S"), *a, flush=True)


class AuthError(Exception):
    pass


class Intra:
    def __init__(self, cookie_file, use_browser=True):
        self.cookie_file, self.use_browser = cookie_file, use_browser
        self.csrf = None
        self.last_refresh = 0.0
        if not os.path.exists(cookie_file):
            self.refresh_cookies("no cookie file")
        self._load_jar()

    def _load_jar(self):
        self.jar = http.cookiejar.MozillaCookieJar(self.cookie_file)
        self.jar.load(ignore_discard=True, ignore_expires=True)
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        ua_file = self.cookie_file + ".ua"  # cf_clearance only works with the UA that earned it
        self.ua = open(ua_file).read().strip() if os.path.exists(ua_file) else os.environ.get("FT_UA", DEFAULT_UA)

    def refresh_cookies(self, why, **kw):
        if not self.use_browser:
            raise AuthError(f"{why}; re-export cookies (browser refresh disabled)")
        if time.time() - self.last_refresh < 120:
            raise AuthError(f"{why}, right after a browser refresh; run browser_cookies.py --login")
        log(f"{why} -> refreshing cookies with Chrome")
        from browser_cookies import refresh_cookies
        refresh_cookies(self.cookie_file, **kw)
        self.last_refresh = time.time()
        if hasattr(self, "jar"):
            self._load_jar()

    def req(self, path, data=None, headers=None, _retry=True):
        h = {"User-Agent": self.ua, **(headers or {})}
        body = urllib.parse.urlencode(data).encode() if data is not None else None
        r = urllib.request.Request(BASE + path, data=body, headers=h, method="POST" if body else "GET")
        try:
            with self.opener.open(r, timeout=30) as resp:
                out = resp.status, resp.geturl(), resp.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as e:
            out = e.code, e.geturl(), e.read().decode("utf-8", "replace")
        self.jar.save(ignore_discard=True, ignore_expires=True)  # keep rotated session cookie
        code, url, body = out
        cf = code in (403, 503) and ("Just a moment" in body or "cf-chl" in body or "challenge-platform" in body)
        if _retry and (cf or "signin.intra.42.fr" in url or "auth.42.fr" in url or code == 401):
            self.refresh_cookies("Cloudflare challenge" if cf else "logged out")
            return self.req(path, data, headers, _retry=False)
        return out

    def load_page(self, slug, team_id=None):
        """Scrape the booking page for team_id, duration, update url and CSRF token."""
        q = f"?team_id={team_id}" if team_id else ""
        code, url, html = self.req(f"/projects/{slug}/slots{q}")
        if "signin.intra.42.fr" in url or "auth.42.fr" in url or code in (401, 403):
            raise AuthError(f"not logged in (HTTP {code} {url})")
        m = re.search(r"<div ([^>]*)id='calendar'", html)
        if code != 200 or not m:
            raise RuntimeError(f"no booking calendar on slots page (HTTP {code} {url}); "
                               "is the project waiting for correction?")
        attrs = dict(re.findall(r"data-([\w-]+)='([^']*)'", m.group(1)))
        csrf = re.search(r'name="csrf-token" content="([^"]+)"', html)
        self.csrf = csrf.group(1) if csrf else None
        tid = re.search(r"team_id=(\d+)", attrs["index-url"]).group(1)
        return {"team_id": tid, "duration": int(attrs.get("duration", 2)),
                "index_url": attrs["index-url"], "update_url": attrs["update-url"]}

    def whoami(self):
        """Login of the session owner, or None if the session is gone (no browser refresh)."""
        code, url, html = self.req("/", _retry=False)
        m = re.search(r'"login":\s*"([^"]+)"', html) if code == 200 and url.startswith(BASE) else None
        return m.group(1) if m else None

    def open_projects(self):
        """Projects with an open team, from the intra home page: [{slug, name, status}]."""
        code, _, html = self.req("/")
        out = []
        for m in re.finditer(r'<a class="simple-link[^"]*" href="/([^/"]+)/[^"]+">(.*?)</a>', html, re.S):
            name = re.search(r"<b[^>]*>(.*?)</b>", m.group(2), re.S)
            status = re.search(r"<small>(.*?)</small>", m.group(2), re.S)
            out.append({"slug": m.group(1), "name": htmllib.unescape(name.group(1).strip()) if name else m.group(1),
                        "status": htmllib.unescape(status.group(1).strip()) if status else ""})
        return out

    def upcoming_evaluations(self, slug):
        """Start times of evaluations already booked for you on this project."""
        code, _, html = self.req(f"/{slug}/mine")
        out = []
        for block in re.split(r"(?=<div class='project-item reminder')", html)[1:]:
            if "You will be evaluated" not in block[:600]:
                continue
            d = re.search(r"data-long-date='([^']+)'", block)
            if d:
                out.append(dt.datetime.strptime(d.group(1), "%Y-%m-%d %H:%M:%S %z"))
        return sorted(out)

    def json_headers(self, referer):
        h = {"Accept": "application/json, text/javascript, */*; q=0.01",
             "X-Requested-With": "XMLHttpRequest", "Referer": referer}
        if self.csrf:
            h["X-CSRF-Token"] = self.csrf
        return h


def candidates(events, duration, not_before, before, avoid=()):
    """Expand available blocks into bookable (start, ids) choices within the window, earliest first."""
    out = []
    for ev in events:
        if ev.get("title") != "Available":
            continue
        ids = ev["ids"].split(",")
        start = dt.datetime.fromisoformat(ev["start"])
        for i in range(len(ids) - duration + 1):
            s = start + dt.timedelta(minutes=SLOT_MIN * i)
            e = s + dt.timedelta(minutes=SLOT_MIN * duration)
            if not_before <= s < before and not any(s < a_end and e > a_start for a_start, a_end in avoid):
                out.append((s, ",".join(ids[i:i + duration])))
    return sorted(out)


def parse_time(v, now):
    """'02:00' -> next occurrence of that local time; otherwise an ISO datetime."""
    if re.fullmatch(r"\d{1,2}:\d{2}", v):
        h, m = map(int, v.split(":"))
        t = now.replace(hour=h, minute=m, second=0, microsecond=0)
        return t if t > now else t + dt.timedelta(days=1)
    t = dt.datetime.fromisoformat(v)
    return t if t.tzinfo else t.astimezone()


def book_evaluations(intra, project, before, count=1, after=None, lead=0, interval=15.0, avoid=(),
                     buffer=15, dry_run=False, team_id=None, stop=None, on_booked=None):
    """Poll until `count` evaluations are booked, `before` passes, or `stop` is set.

    Each slot must start in [max(now+lead, after), before) and stay `buffer` minutes clear of
    `avoid` ranges, of evaluations already booked on the project, and of each other.
    Returns the list of booked (start, end).
    """
    stop = stop or threading.Event()
    avoid = list(avoid)
    booked = []
    pad = dt.timedelta(minutes=buffer)

    def busy():
        try:
            existing = intra.upcoming_evaluations(project)
        except Exception as e:
            log(f"could not read existing evaluations ({e})")
            existing = []
        rng = [(s - pad, s + dt.timedelta(minutes=SLOT_MIN * page["duration"]) + pad) for s in existing]
        return existing, rng

    page = intra.load_page(project, team_id)
    referer = f"{BASE}/projects/{project}/slots?team_id={page['team_id']}"
    existing, auto_avoid = busy()
    log(f"{project}: team {page['team_id']}, evaluation = {page['duration']} x {SLOT_MIN} min")
    for s in existing:
        log(f"already booked: {s.astimezone():%a %d %H:%M} (keeping {buffer} min clear)")
    log(f"looking for {count} slot(s) starting before {before:%a %d %H:%M}")
    rejected = set()

    while len(booked) < count and not stop.is_set():
        now = dt.datetime.now().astimezone()
        if now >= before:
            log("deadline passed." if booked else "deadline passed, nothing booked.")
            break
        not_before = max(after or now, now + dt.timedelta(minutes=lead))
        window = {"start": now.date().isoformat(), "end": (before.date() + dt.timedelta(days=1)).isoformat()}
        code, _, body = intra.req(f"{page['index_url']}&{urllib.parse.urlencode(window)}",
                                  headers=intra.json_headers(referer))
        events = None
        if code == 429:
            log("rate limited, backing off 60s")
            stop.wait(60)
            continue
        if code == 200:
            try:
                events = json.loads(body)
            except ValueError:
                log("non-JSON slots response, reloading page")
        else:
            log(f"slots fetch failed HTTP {code}")
        if events is None:
            page = intra.load_page(project, team_id)
            stop.wait(interval)
            continue

        choices = [c for c in candidates(events, page["duration"], not_before, before, avoid + auto_avoid)
                   if c[1] not in rejected]
        if not choices:
            log(f"no slot in window ({sum(e.get('title') == 'Available' for e in events)} blocks open overall)")
        for start, ids in choices:
            if stop.is_set():
                break
            end = start + dt.timedelta(minutes=SLOT_MIN * page["duration"])
            if dry_run:
                log(f"[dry-run] would book {start:%a %d %H:%M}-{end:%H:%M}")
                booked.append((start, end))
                auto_avoid.append((start - pad, end + pad))
                if on_booked:
                    on_booked(start, end)
                if len(booked) >= count:
                    break
                continue
            log(f"booking {start:%a %d %H:%M}-{end:%H:%M} ...")
            code, _, body = intra.req(page["update_url"].replace(":ids", ids),
                                      data={**window, "_method": "put"}, headers=intra.json_headers(referer))
            try:
                res = json.loads(body)
            except ValueError:
                res = {"status": code, "message": body[:200]}
            if code == 200 and res.get("status", 200) == 200:
                log(f"BOOKED {project} evaluation {start:%a %d %H:%M}-{end:%H:%M}: {res.get('message', '')}")
                booked.append((start, end))
                if on_booked:
                    on_booked(start, end)
                auto_avoid.append((start - pad, end + pad))
                if len(booked) < count:
                    try:
                        page = intra.load_page(project, team_id)
                    except RuntimeError as e:
                        log(f"can't book more: {e}")
                        return booked
                    existing, rng = busy()
                    auto_avoid.extend(rng)
                break  # fresh slot list before the next booking
            log(f"booking refused (HTTP {code}): {res.get('message', res)}")
            if code == 422 and "authenticity" in body.lower():
                page = intra.load_page(project, team_id)  # stale CSRF, retry next round
                break
            rejected.add(ids)
        if len(booked) < count:
            stop.wait(interval + random.uniform(0, interval * 0.3))
    return booked


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("project", help="project slug, e.g. codexion")
    when = ap.add_mutually_exclusive_group(required=True)
    when.add_argument("--before", help="slot must START before this: 'HH:MM' (next occurrence, local) or ISO datetime")
    when.add_argument("--hours", type=float, help="slot must start within this many hours from now")
    ap.add_argument("--count", type=int, default=1, help="how many evaluations to book (default 1)")
    ap.add_argument("--after", help="slot must start at/after this (same formats); default: now + --lead")
    ap.add_argument("--lead", type=int, default=0, help="minimum minutes from now before the slot starts (default 0)")
    ap.add_argument("--interval", type=float, default=15, help="seconds between polls (default 15)")
    ap.add_argument("--avoid", action="append", default=[], metavar="FROM,TO",
                    help="don't book anything overlapping this range ('HH:MM' or ISO, comma-separated); repeatable")
    ap.add_argument("--buffer", type=int, default=15, help="minutes kept clear around other evaluations (default 15)")
    ap.add_argument("--cookies", default=os.path.join(HERE, "cookies42.txt"))
    ap.add_argument("--team-id", help="override team id (default: whatever the intra page uses)")
    ap.add_argument("--no-browser", action="store_true", help="never launch Chrome to refresh cookies")
    ap.add_argument("--dry-run", action="store_true", help="show the slots that would be booked, don't book")
    a = ap.parse_args()

    now = dt.datetime.now().astimezone()
    before = now + dt.timedelta(hours=a.hours) if a.hours is not None else parse_time(a.before, now)
    avoid = [tuple(parse_time(x.strip(), now) for x in r.split(",")) for r in a.avoid]
    for a_start, a_end in avoid:
        log(f"avoiding {a_start:%a %d %H:%M}-{a_end:%H:%M}")
    intra = Intra(a.cookies, use_browser=not a.no_browser)
    booked = book_evaluations(intra, a.project, before, count=a.count,
                              after=parse_time(a.after, now) if a.after else None, lead=a.lead,
                              interval=a.interval, avoid=avoid, buffer=a.buffer, dry_run=a.dry_run,
                              team_id=a.team_id)
    log(f"done: {len(booked)}/{a.count} booked")
    return 0 if len(booked) == a.count else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except AuthError as e:
        sys.exit(f"auth failed: {e}")
    except KeyboardInterrupt:
        sys.exit(130)
