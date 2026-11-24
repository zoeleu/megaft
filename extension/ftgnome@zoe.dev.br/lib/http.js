// -*- mode: js; js-indent-level: 4; indent-tabs-mode: nil -*-
/* JSON-over-HTTP helper that works with both libsoup 2.4 (GNOME 42) and
 * libsoup 3 (GNOME 43+). Legacy (non-ESM) module: exports are `var`s.
 * Has no shell UI imports, so it also runs under plain `gjs` for tests. */
'use strict';

const { GLib, Soup } = imports.gi;

const SOUP3 = Soup.MAJOR_VERSION >= 3;

var HttpError = class HttpError extends Error {
    constructor(status, body, message) {
        super(message);
        this.name = 'HttpError';
        this.status = status;   // 0 = network error
        this.body = body;       // parsed JSON body (or null)
    }

    /** OAuth-style error code from the body, e.g. "authorization_pending". */
    get code() {
        return this.body && typeof this.body.error === 'string' ? this.body.error : null;
    }
};

/** Only https, or plain http to the local machine. Returns the normalised base URL or null. */
function normalizeBaseUrl(input) {
    if (typeof input !== 'string')
        return null;
    let uri;
    try {
        uri = GLib.Uri.parse(input.trim(), GLib.UriFlags.NONE);
    } catch (e) {
        return null;
    }
    const scheme = uri.get_scheme();
    const host = uri.get_host();
    if (!host || uri.get_query() || uri.get_fragment() || uri.get_userinfo())
        return null;
    const local = ['localhost', '127.0.0.1', '::1', '[::1]'].includes(host);
    if (scheme !== 'https' && !(scheme === 'http' && local))
        return null;
    return input.trim().replace(/\/+$/, '');
}

function decode(bytes) {
    if (!bytes)
        return '';
    const data = bytes instanceof Uint8Array ? bytes : bytes.get_data();
    return data ? new TextDecoder().decode(data) : '';
}

var Http = class Http {
    constructor(userAgent) {
        this._session = new Soup.Session({ timeout: 20 });
        this._session.user_agent = userAgent;
        this._destroyed = false;
    }

    /**
     * @param {string} method
     * @param {string} url
     * @param {{json?: object, bearer?: string}} [opts]
     * @returns {Promise<object|null>} parsed JSON body of a 2xx response
     * @throws {HttpError}
     */
    request(method, url, opts = {}) {
        if (this._destroyed)
            return Promise.reject(new HttpError(0, null, 'HTTP client destroyed'));

        const msg = Soup.Message.new(method, url);
        if (!msg)
            return Promise.reject(new HttpError(0, null, `Invalid URL: ${url}`));
        msg.request_headers.append('Accept', 'application/json');
        if (opts.bearer)
            msg.request_headers.append('Authorization', `Bearer ${opts.bearer}`);

        const body = opts.json !== undefined ? JSON.stringify(opts.json) : null;

        return new Promise((resolve, reject) => {
            const finish = (status, text) => {
                let parsed = null;
                try {
                    parsed = text ? JSON.parse(text) : null;
                } catch (e) {
                    parsed = null;
                }
                if (status >= 200 && status < 300) {
                    resolve(parsed);
                } else {
                    const desc = parsed && (parsed.error_description || parsed.error);
                    reject(new HttpError(status, parsed, desc ? String(desc) : `HTTP ${status}`));
                }
            };

            if (SOUP3) {
                if (body !== null) {
                    msg.set_request_body_from_bytes('application/json',
                        new GLib.Bytes(new TextEncoder().encode(body)));
                }
                this._session.send_and_read_async(msg, GLib.PRIORITY_DEFAULT, null, (session, res) => {
                    let bytes;
                    try {
                        bytes = session.send_and_read_finish(res);
                    } catch (e) {
                        reject(new HttpError(0, null, `Network error: ${e.message}`));
                        return;
                    }
                    finish(msg.get_status(), decode(bytes));
                });
            } else {
                if (body !== null)
                    msg.set_request('application/json', Soup.MemoryUse.COPY, new TextEncoder().encode(body));
                this._session.queue_message(msg, (_session, m) => {
                    // Soup 2.4 reports transport failures as status codes < 100
                    if (m.status_code < 100) {
                        reject(new HttpError(0, null, `Network error: ${Soup.Status.get_phrase(m.status_code) || m.status_code}`));
                        return;
                    }
                    finish(m.status_code, m.response_body ? m.response_body.data : '');
                });
            }
        });
    }

    destroy() {
        this._destroyed = true;
        this._session.abort();
    }
};
