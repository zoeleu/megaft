// Integration test for lib/megaftd.js against a running/activatable megaftd (dry run, books nothing).
// Usage: gjs extension/test-megaftd.js
const { Gio, GLib } = imports.gi;
const here = GLib.path_get_dirname(GLib.canonicalize_filename(imports.system.programInvocationName, null));
const ext = GLib.build_filenamev([here, 'ftgnome@zoe.dev.br']);
imports.searchPath.unshift(ext);
const Megaftd = imports.lib.megaftd;

const loop = new GLib.MainLoop(null, false);
let rc = 1;
let jobPath = null;
let finished = false;
const booked = [];
const fail = msg => { printerr(`FAIL: ${msg}`); loop.quit(); };

// The watcher never starts megaftd itself; make sure it's up for the test.
Gio.DBus.session.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
    'StartServiceByName', new GLib.Variant('(su)', [Megaftd.BUS_NAME, 0]), null, Gio.DBusCallFlags.NONE, -1, null);

const w = new Megaftd.Watcher(GLib.build_filenamev([ext, 'dbus', 'megaft1.xml']), {
    onAvailable: up => {
        print(`megaftd available: ${up}`);
        if (!up)
            return;
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, 500, () => {
            const deadline = Math.floor(Date.now() / 1000) + 72 * 3600;
            w._evaluations.call('CreateJob', new GLib.Variant('(suxub)', ['codexion', 2, deadline, 15, true]),
                Gio.DBusCallFlags.NONE, -1, null, (p, res) => {
                    try {
                        [jobPath] = p.call_finish(res).deep_unpack();
                        print(`created ${jobPath}`);
                    } catch (e) {
                        fail(`CreateJob: ${e.message}`);
                    }
                });
            return GLib.SOURCE_REMOVE;
        });
    },
    onBooked: (job, start, end) => {
        booked.push(Megaftd.formatSpan(start, end));
        print(`Booked signal: ${job.project} ${Megaftd.formatSpan(start, end)} (dry run: ${job.dryRun})`);
    },
    onChange: jobs => {
        const j = jobs.find(x => x.path === jobPath);
        if (finished || !j || j.state === 'running' || j.state === '')
            return;
        finished = true;
        print(`job ${j.path}: state=${j.state} booked=${j.booked.length}/${j.count} last="${j.lastLog}"`);
        if (j.state !== 'done' || j.booked.length !== 2 || booked.length !== 2)
            return fail(`expected done with 2 slots and 2 Booked signals, got ${j.state}, ${j.booked.length}, ${booked.length}`);
        w.dismiss(j.path).then(() => {
            print('dismissed');
            rc = 0;
            loop.quit();
        }).catch(e => fail(`Dismiss: ${e.message}`));
    },
});
GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 90, () => fail('timeout'));
loop.run();
w.destroy();
print(rc === 0 ? 'PASS' : 'FAIL');
imports.system.exit(rc);
