// -*- mode: js; js-indent-level: 4; indent-tabs-mode: nil -*-
/* ft_gnome server client: device linking (RFC 8628 device flow), token
 * refresh with rotation, and the device API. Holds no 42 secrets — those
 * stay on the server.
 *
 * Usage (shell or plain gjs, after putting the extension dir on the search path):
 *   const client = new Client.FtClient({ http, store, getBaseUrl, deviceName, platform });
 */
'use strict';

const { GLib } = imports.gi;

const DEVICE_GRANT = 'urn:ietf:params:oauth:grant-type:device_code';

var NotLinkedError = class NotLinkedError extends Error {
    constructor(message = 'This computer is not linked') {
        super(message);
        this.name = 'NotLinkedError';
    }
};

var FtClient = class FtClient {
    /**
     * @param {object} p
     * @param {object} p.http        lib/http.js Http instance
     * @param {object} p.store       lib/secrets.js module (load/save/clear)
     * @param {() => string|null} p.getBaseUrl  validated server URL
     * @param {() => string} p.deviceName
     * @param {string} p.platform
     */
    constructor({ http, store, getBaseUrl, deviceName, platform }) {
        this._http = http;
        this._store = store;
        this._getBaseUrl = getBaseUrl;
        this._deviceName = deviceName;
        this._platform = platform;
        this._creds = null;
        this._credsFor = null;
        this._refreshing = null;
        this._linkGeneration = 0;
        this._destroyed = false;
        this._timers = new Map(); // source id -> resolve, so destroy() can remove them
        this.onUnlinked = null; // called when the server revoked us
    }

    /** Resolves true after `ms`, or false if cancelled / destroyed meanwhile. */
    _sleep(ms, isCancelled) {
        return new Promise(resolve => {
            const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
                this._timers.delete(id);
                resolve(!isCancelled());
                return GLib.SOURCE_REMOVE;
            });
            this._timers.set(id, resolve);
        });
    }

    _base() {
        const base = this._getBaseUrl();
        if (!base)
            throw new Error('Server URL is invalid: it must be https:// (or http://localhost). Fix it in the extension settings.');
        return base;
    }

    /** Loads stored credentials for the current server URL. */
    async load() {
        const base = this._getBaseUrl();
        this._credsFor = base;
        this._creds = base ? await this._store.load(base) : null;
        return this.isLinked();
    }

    isLinked() {
        return !!(this._creds && this._creds.refresh_token && this._credsFor === this._getBaseUrl());
    }

    get deviceId() {
        return this._creds ? this._creds.device_id : null;
    }

    async _saveTokens(base, t) {
        this._creds = {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            access_expires_at: Date.now() + (t.expires_in || 3600) * 1000,
            device_id: t.device_id,
        };
        this._credsFor = base;
        await this._store.save(base, this._creds);
    }

    async _forget(notify) {
        const base = this._credsFor;
        this._creds = null;
        if (base)
            await this._store.clear(base);
        if (notify && this.onUnlinked)
            this.onUnlinked();
    }

    // ── Linking ─────────────────────────────────────────────────────────────

    /**
     * Starts the device flow. Returns the code to show the user plus a
     * `wait()` promise that resolves to true once approved (false if cancelled).
     */
    async startLink() {
        const base = this._base();
        const gen = ++this._linkGeneration;
        const req = await this._http.request('POST', `${base}/api/v1/device/code`, {
            json: { name: this._deviceName(), platform: this._platform },
        });

        const cancelled = () => this._destroyed || gen !== this._linkGeneration;
        const deadline = Date.now() + req.expires_in * 1000;
        const wait = async () => {
            let interval = Math.max(req.interval || 5, 1);
            while (Date.now() < deadline) {
                if (!await this._sleep(interval * 1000, cancelled))
                    return false;
                try {
                    const tokens = await this._http.request('POST', `${base}/api/v1/device/token`, {
                        json: { grant_type: DEVICE_GRANT, device_code: req.device_code },
                    });
                    if (cancelled())
                        return false;
                    await this._saveTokens(base, tokens);
                    return true;
                } catch (e) {
                    if (e.code === 'authorization_pending')
                        continue;
                    if (e.code === 'slow_down') {
                        interval += 5;
                        continue;
                    }
                    if (e.status === 0 || e.status === 429 || e.status >= 500)
                        continue; // transient — keep polling until the deadline
                    if (e.code === 'access_denied')
                        throw new Error('The link request was denied in the browser.');
                    throw e;
                }
            }
            throw new Error('The code expired before it was approved. Try linking again.');
        };

        return {
            userCode: req.user_code,
            verificationUri: req.verification_uri,
            verificationUriComplete: req.verification_uri_complete,
            wait,
        };
    }

    cancelLink() {
        this._linkGeneration++;
    }

    // ── Tokens ──────────────────────────────────────────────────────────────

    async _refresh() {
        // one refresh at a time: the server rotates refresh tokens and treats
        // reuse of an old one as theft, so concurrent refreshes must not happen
        if (!this._refreshing) {
            this._refreshing = (async () => {
                const base = this._credsFor;
                try {
                    const t = await this._http.request('POST', `${base}/api/v1/device/token`, {
                        json: { grant_type: 'refresh_token', refresh_token: this._creds.refresh_token },
                    });
                    await this._saveTokens(base, t);
                } catch (e) {
                    if (e.code === 'invalid_grant') {
                        await this._forget(true);
                        throw new NotLinkedError('This computer was unlinked or revoked. Link it again from the menu.');
                    }
                    throw e;
                } finally {
                    this._refreshing = null;
                }
            })();
        }
        return this._refreshing;
    }

    async _accessToken() {
        if (!this.isLinked())
            throw new NotLinkedError();
        if (this._creds.access_expires_at - Date.now() < 60000)
            await this._refresh();
        return this._creds.access_token;
    }

    async _api(method, path, json) {
        const base = this._base();
        let token = await this._accessToken();
        try {
            return await this._http.request(method, `${base}/api/v1${path}`, { bearer: token, json });
        } catch (e) {
            if (e.status !== 401)
                throw e;
        }
        // access token rejected: refresh once and retry
        await this._refresh();
        token = this._creds.access_token;
        try {
            return await this._http.request(method, `${base}/api/v1${path}`, { bearer: token, json });
        } catch (e) {
            if (e.status === 401) {
                await this._forget(true);
                throw new NotLinkedError('This computer was revoked. Link it again from the menu.');
            }
            throw e;
        }
    }

    // ── API ─────────────────────────────────────────────────────────────────

    summary() {
        return this._api('GET', '/summary');
    }

    async devices() {
        return (await this._api('GET', '/devices')).devices;
    }

    revokeDevice(id) {
        return this._api('DELETE', `/devices/${encodeURIComponent(id)}`);
    }

    renameDevice(id, name) {
        return this._api('PATCH', `/devices/${encodeURIComponent(id)}`, { name });
    }

    /** Revoke this device on the server (best effort) and forget local credentials. */
    async unlink() {
        try {
            if (this.isLinked())
                await this._api('POST', '/device/revoke');
        } catch (e) {
            log(`[ftgnome] server-side unlink failed (credentials removed locally anyway): ${e.message}`);
        }
        await this._forget(false);
    }

    destroy() {
        this._destroyed = true;
        this._linkGeneration++;
        this.onUnlinked = null;
        for (const [id, resolve] of this._timers) {
            GLib.source_remove(id);
            resolve(false);
        }
        this._timers.clear();
    }
};

// ── Display helpers (shared by extension.js and tests) ──────────────────────

/** "3h05" from seconds */
function formatDuration(seconds) {
    const total = Math.max(0, Math.round(seconds / 60));
    const h = Math.floor(total / 60);
    const m = total % 60;
    return `${h}h${m < 10 ? '0' : ''}${m}`;
}

/** Sums the server's per-day logtime map into { today, week } seconds (week starts Monday, local time). */
function logtimeTotals(logtime, now = GLib.DateTime.new_now_local()) {
    const map = logtime || {};
    const today = now.format('%Y-%m-%d');
    let week = 0;
    const daysSinceMonday = now.get_day_of_week() - 1; // 1 = Monday … 7 = Sunday
    for (let i = 0; i <= daysSinceMonday; i++)
        week += map[now.add_days(-i).format('%Y-%m-%d')] || 0;
    return { today: map[today] || 0, week };
}

/** Local "Mon 14:30" (or "14:30" if today) for an ISO timestamp. */
function formatWhen(iso, now = GLib.DateTime.new_now_local()) {
    const dt = GLib.DateTime.new_from_iso8601(iso, null);
    if (!dt)
        return iso;
    const local = dt.to_local();
    const sameDay = local.format('%Y-%m-%d') === now.format('%Y-%m-%d');
    return local.format(sameDay ? '%H:%M' : '%a %H:%M');
}
