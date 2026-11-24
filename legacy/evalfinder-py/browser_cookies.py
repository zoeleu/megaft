#!/usr/bin/env python3
"""Get fresh 42 intra cookies (incl. Cloudflare's cf_clearance) with undetected-chromedriver.

Credentials are never saved. The only thing kept is the intra session in cookies42.txt (home, 0600).
Chrome runs in a throwaway profile under /tmp, deleted after each run; the session is loaded into it
from cookies42.txt. If the session is gone, it logs in with the password the GUI holds in memory
(or FT_LOGIN / FT_PASSWORD from the environment); otherwise a visible window opens for you.

  .venv/bin/python browser_cookies.py --login   # visible window, log in yourself
  .venv/bin/python browser_cookies.py           # headless refresh of cookies42.txt
"""
import argparse, os, shutil, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
STORE = os.environ.get("EVALFINDER_STORE", "/sgoinfre/zleullie/evalfinder")
DRIVER_DIR = os.path.join(STORE, "chromedriver")  # just the chromedriver binary, nothing secret
DEFAULT_COOKIES = os.path.join(HERE, "cookies42.txt")
TARGET = "https://projects.intra.42.fr/"
LOG_HOOK = None  # set by the GUI to capture output
CREDENTIALS = None  # (login, password) held in memory by the GUI; never written anywhere


class BadCredentials(Exception):
    pass


def log(*a):
    if LOG_HOOK:
        LOG_HOOK("[browser] " + " ".join(map(str, a)))
    else:
        print(time.strftime("%H:%M:%S"), "[browser]", *a, file=sys.stderr, flush=True)


def load_dotenv():
    p = os.path.join(HERE, ".env")
    if os.path.exists(p):
        for line in open(p):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip().strip("'\""))


def _credentials():
    if CREDENTIALS:
        return CREDENTIALS
    return os.environ.get("FT_LOGIN"), os.environ.get("FT_PASSWORD")


def _start(headless, profile):
    import undetected_chromedriver as uc
    uc.Patcher.data_path = DRIVER_DIR  # keep the downloaded chromedriver out of $HOME
    opts = uc.ChromeOptions()
    opts.add_argument("--no-first-run")
    opts.add_argument("--password-store=basic")
    opts.add_argument("--window-size=1200,900")
    return uc.Chrome(options=opts, user_data_dir=profile, headless=headless, use_subprocess=True)


def _logged_in(drv):
    try:
        return (drv.current_url.startswith(TARGET)
                and drv.execute_script("return !!(window._user && window._user.login)"))
    except Exception:
        return False


def _try_password_login(drv):
    """Fill the auth.42.fr (Keycloak) form if credentials are available. Returns True if submitted."""
    from selenium.webdriver.common.by import By
    login, pw = _credentials()
    if not (login and pw):
        return False
    try:
        user = drv.find_element(By.ID, "username")
        user.clear(); user.send_keys(login)
        p = drv.find_element(By.ID, "password")
        p.clear(); p.send_keys(pw)
        try:
            drv.find_element(By.ID, "rememberMe").click()
        except Exception:
            pass
        drv.find_element(By.ID, "kc-login").click()
        log("submitted login form")
        return True
    except Exception as e:
        log(f"login form not found/filled ({type(e).__name__})")
        return False


def _login_error(drv):
    from selenium.webdriver.common.by import By
    for el in drv.find_elements(By.CSS_SELECTOR, "#input-error, .kc-feedback-text, .alert-error, .pf-m-danger"):
        t = el.text.strip()
        if t:
            return t
    return None


def _wait_logged_in(drv, seconds, check_errors=True):
    end = time.time() + seconds
    tried_pw = False
    while time.time() < end:
        if _logged_in(drv):
            return True
        if "auth.42.fr" in drv.current_url:
            if not tried_pw:
                tried_pw = _try_password_login(drv) or True
                time.sleep(2)
            elif check_errors:
                err = _login_error(drv)
                if err:
                    raise BadCredentials(err)
        time.sleep(1)
    return False


def _restore(drv, cookie_file):
    """Load the saved session (Netscape cookies.txt) into the throwaway profile."""
    if not os.path.exists(cookie_file):
        return
    cookies = []
    for line in open(cookie_file):
        parts = line.rstrip("\n").split("\t")
        if line.startswith("#") or len(parts) != 7:
            continue
        domain, _, path, secure, exp, name, value = parts
        c = {"domain": domain, "path": path, "secure": secure == "TRUE", "name": name, "value": value}
        if int(exp or 0):
            c["expires"] = int(exp)
        cookies.append(c)
    try:
        drv.execute_cdp_cmd("Network.enable", {})
        drv.execute_cdp_cmd("Network.setCookies", {"cookies": cookies})
    except Exception as e:
        log(f"could not restore saved cookies ({e})")


def _dump(drv, cookie_file):
    cookies = drv.execute_cdp_cmd("Network.getAllCookies", {})["cookies"]
    mine = [c for c in cookies if c["domain"] == "42.fr" or c["domain"].endswith(".42.fr")]
    lines = ["# Netscape HTTP Cookie File"]
    for c in mine:
        d = c["domain"]
        exp = int(c.get("expires", 0)) if not c.get("session") and c.get("expires", -1) > 0 else 0
        lines.append("\t".join([d, "TRUE" if d.startswith(".") else "FALSE", c["path"],
                                "TRUE" if c["secure"] else "FALSE", str(exp), c["name"], c["value"]]))
    ua = drv.execute_script("return navigator.userAgent").replace("HeadlessChrome", "Chrome")
    tmp = cookie_file + ".tmp"
    with open(os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as f:
        f.write("\n".join(lines) + "\n")
    os.replace(tmp, cookie_file)
    with open(cookie_file + ".ua", "w") as f:  # cf_clearance is bound to this user agent
        f.write(ua)
    log(f"saved {len(mine)} cookies")
    return ua


def _wipe(profile):
    """Delete the throwaway profile; Chrome may still be flushing files right after quit()."""
    for _ in range(20):
        shutil.rmtree(profile, ignore_errors=True)
        if not os.path.exists(profile):
            return
        time.sleep(0.5)
    log(f"warning: could not fully delete {profile}")


def refresh_cookies(cookie_file=DEFAULT_COOKIES, headless=True, login_wait=300, fresh=False,
                    allow_visible=True):
    """Open intra in Chrome, make sure we're logged in, write cookies + UA. Returns the login.

    fresh=True ignores the saved session so the password is really used (and checked);
    a rejected password then raises BadCredentials instead of falling back to a visible window.
    """
    load_dotenv()
    modes = [True, False] if headless else [False]
    if not allow_visible:
        modes = [m for m in modes if m] or [True]
    for attempt_headless in modes:
        log(f"starting Chrome ({'headless' if attempt_headless else 'visible'})")
        profile = tempfile.mkdtemp(prefix="evalfinder-chrome-")  # 0700, local disk, removed below
        drv = None
        try:
            drv = _start(attempt_headless, profile)
            if not fresh:
                _restore(drv, cookie_file)
            drv.get(TARGET)
            wait = 45 if attempt_headless else login_wait
            if not attempt_headless and not _logged_in(drv):
                log(f"log in to intra in the Chrome window (waiting up to {login_wait}s)")
            try:
                ok = _wait_logged_in(drv, wait, check_errors=fresh or attempt_headless)
            except BadCredentials as e:
                if fresh or not allow_visible or not attempt_headless:
                    raise
                log(f"saved password rejected ({e}); falling back to a visible window")
                continue
            if ok:
                _dump(drv, cookie_file)
                return drv.execute_script("return window._user.login")
            log(f"not logged in (stuck at {drv.current_url.split('?')[0]}, title {drv.title!r})")
        finally:
            if drv:
                try:
                    drv.quit()
                except Exception:
                    pass
            _wipe(profile)
    raise RuntimeError("could not log in to intra")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--login", action="store_true", help="open a visible window to log in by hand")
    ap.add_argument("--cookies", default=DEFAULT_COOKIES)
    a = ap.parse_args()
    print("logged in as", refresh_cookies(a.cookies, headless=not a.login))
