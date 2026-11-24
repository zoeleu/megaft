#!/usr/bin/env -S gjs
// Integration test: drives lib/http.js + lib/client.js (exactly as the shell
// does) against scripts/fake-server.ts. Usage: gjs extension/test-client.js URL
'use strict';
imports.gi.versions.Soup = '2.4';
const { GLib, Soup } = imports.gi;
const dir = GLib.path_get_dirname(GLib.canonicalize_filename(imports.system.programInvocationName, null));
imports.searchPath.unshift(`${dir}/ftgnome@zoe.dev.br`);
const Http = imports.lib.http;
const Client = imports.lib.client;

const BASE = ARGV[0];
const mem = new Map();
const store = {
    load: k => Promise.resolve(mem.has(k) ? JSON.parse(mem.get(k)) : null),
    save: (k, v) => Promise.resolve(void mem.set(k, JSON.stringify(v))),
    clear: k => Promise.resolve(void mem.delete(k)),
};
let failures = 0;
const check = (cond, msg) => { print(`${cond ? 'ok  ' : 'FAIL'} ${msg}`); if (!cond) failures++; };

const loop = GLib.MainLoop.new(null, false);
(async () => {
    print(`Soup ${Soup.MAJOR_VERSION}.${Soup.MINOR_VERSION}`);
    check(Http.normalizeBaseUrl('http://evil.com') === null, 'rejects plain http to remote hosts');
    check(Http.normalizeBaseUrl('https://x.dev/') === 'https://x.dev', 'accepts https');
    check(Http.normalizeBaseUrl('http://localhost:3000') === 'http://localhost:3000', 'accepts http://localhost');

    const http = new Http.Http('ftgnome-test');
    const mk = () => new Client.FtClient({ http, store, getBaseUrl: () => BASE, deviceName: () => 'gjs-test', platform: 'GNOME Shell 42 (test)' });
    const client = mk();
    check(!(await client.load()), 'starts unlinked');

    const link = await client.startLink();
    check(/^[A-Z]{4}-[A-Z]{4}$/.test(link.userCode), `got user code ${link.userCode}`);
    const waiting = link.wait();
    // approve from the "browser" after one pending poll
    GLib.timeout_add(GLib.PRIORITY_DEFAULT, 6000, () => {
        http.request('POST', `${BASE}/__test/approve`, { json: { user_code: link.userCode } });
        return GLib.SOURCE_REMOVE;
    });
    check(await waiting === true, 'device flow completed after approval');
    check(client.isLinked() && mem.size === 1, 'credentials stored');

    const s = await client.summary();
    check(s.user.login === 'zleullie', 'summary over bearer token');
    const lt = Client.logtimeTotals(s.logtime, GLib.DateTime.new_local(2026, 11, 7, 12, 0, 0));
    check(lt.today === 4530 && lt.week === 4530 + 12600, `logtime totals (today ${Client.formatDuration(lt.today)}, week ${Client.formatDuration(lt.week)})`);
    check(Client.formatDuration(3599) === '1h00' && Client.formatDuration(65 * 60) === '1h05', 'duration formatting');

    // force an access-token refresh (rotation)
    const oldRefresh = client._creds.refresh_token;
    client._creds.access_expires_at = 0;
    await client.summary();
    check(client._creds.refresh_token !== oldRefresh, 'refresh token rotated');

    const devices = await client.devices();
    check(devices.length === 1 && devices[0].current && devices[0].name === 'gjs-test', 'device list');

    // a second client instance (simulating a restart) loads from the store
    const again = mk();
    check(await again.load(), 'reload from store after restart');

    let unlinkedCb = false;
    client.onUnlinked = () => { unlinkedCb = true; };
    await again.unlink();
    try {
        await client.summary();
        check(false, 'revoked device should fail');
    } catch (e) {
        check(e instanceof Client.NotLinkedError && unlinkedCb && mem.size === 0, `revocation detected: ${e.message}`);
    }

    // network error path
    const dead = new Client.FtClient({ http, store, getBaseUrl: () => 'http://127.0.0.1:1', deviceName: () => 'x', platform: 'x' });
    try {
        await dead.startLink();
        check(false, 'unreachable server should throw');
    } catch (e) {
        check(e.status === 0, `network error surfaced: ${e.message}`);
    }
    client.destroy();
    http.destroy();
})().catch(e => { failures++; print(`FAIL unexpected: ${e}\n${e.stack}`); }).finally(() => loop.quit());
loop.run();
print(failures ? `\n${failures} FAILURE(S)` : '\nall gjs client checks passed');
imports.system.exit(failures ? 1 : 0);
