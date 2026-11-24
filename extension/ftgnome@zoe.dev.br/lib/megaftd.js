// megaftd client for GNOME Shell 42 (legacy imports, no ESM).
//
// Watches br.dev.zoe.MegaFt1 on the session bus *without* starting it, mirrors its booking jobs, and
// reports changes. Proxies are built from dbus/megaft1.xml, the same file megaftd is tested against.
// Only depends on Gio/GLib, so it can be exercised with plain `gjs` (see extension/test-megaftd.js).
/* exported Watcher, BUS_NAME, loadInterfaces */

const { Gio, GLib } = imports.gi;

var BUS_NAME = 'br.dev.zoe.MegaFt1';
const ROOT_PATH = '/br/dev/zoe/MegaFt1';
const EVALUATIONS_IFACE = 'br.dev.zoe.MegaFt1.Evaluations';
const JOB_IFACE = 'br.dev.zoe.MegaFt1.Job';

/** Parses the introspection XML at `path` into a Gio.DBusNodeInfo. */
function loadInterfaces(path) {
    const [ok, bytes] = Gio.File.new_for_path(path).load_contents(null);
    if (!ok)
        throw new Error(`cannot read ${path}`);
    return Gio.DBusNodeInfo.new_for_xml(new TextDecoder().decode(bytes));
}

function newProxy(info, path, iface, cancellable) {
    return new Promise((resolve, reject) => {
        Gio.DBusProxy.new_for_bus(
            Gio.BusType.SESSION,
            Gio.DBusProxyFlags.DO_NOT_AUTO_START,
            info.lookup_interface(iface),
            BUS_NAME, path, iface, cancellable,
            (_src, res) => {
                try {
                    resolve(Gio.DBusProxy.new_for_bus_finish(res));
                } catch (e) {
                    reject(e);
                }
            });
    });
}

function call(proxy, method, params) {
    return new Promise((resolve, reject) => {
        proxy.call(method, params, Gio.DBusCallFlags.NONE, -1, null, (p, res) => {
            try {
                resolve(p.call_finish(res).deep_unpack());
            } catch (e) {
                reject(e);
            }
        });
    });
}

function prop(proxy, name, fallback) {
    const v = proxy.get_cached_property(name);
    return v ? v.deep_unpack() : fallback;
}

/** A plain snapshot of one job, for the UI. */
function snapshot(path, proxy) {
    const booked = prop(proxy, 'Booked', []).map(([s, e]) => ({ start: Number(s), end: Number(e) }));
    return {
        path,
        state: prop(proxy, 'State', ''),
        project: prop(proxy, 'Project', ''),
        count: prop(proxy, 'Count', 0),
        deadline: Number(prop(proxy, 'Deadline', 0)),
        dryRun: prop(proxy, 'DryRun', false),
        booked,
        lastLog: prop(proxy, 'LastLog', ''),
    };
}

/**
 * Mirrors megaftd's jobs.
 *
 * callbacks: { onChange(jobs), onBooked(job, start, end), onAvailable(bool) }
 */
var Watcher = class Watcher {
    constructor(xmlPath, callbacks) {
        this._info = loadInterfaces(xmlPath);
        this._cb = callbacks;
        this._jobs = new Map(); // path -> { proxy, ids }
        this._cancellable = new Gio.Cancellable();
        this._evaluations = null;
        this._evalSignalId = 0;
        this._available = false;
        this._watchId = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME, Gio.BusNameWatcherFlags.NONE,
            () => this._appeared().catch(e => logError(e, 'megaftd')),
            () => this._vanished());
    }

    get available() {
        return this._available;
    }

    jobs() {
        return [...this._jobs.entries()].map(([path, j]) => snapshot(path, j.proxy));
    }

    async cancel(path) {
        const j = this._jobs.get(path);
        if (j)
            await call(j.proxy, 'Cancel', null);
    }

    async dismiss(path) {
        const j = this._jobs.get(path);
        if (j)
            await call(j.proxy, 'Dismiss', null);
    }

    _changed() {
        if (this._cb.onChange)
            this._cb.onChange(this.jobs());
    }

    async _appeared() {
        this._available = true;
        if (this._cb.onAvailable)
            this._cb.onAvailable(true);
        this._evaluations = await newProxy(this._info, ROOT_PATH, EVALUATIONS_IFACE, this._cancellable);
        this._evalSignalId = this._evaluations.connect('g-signal', (_p, _sender, signal, params) => {
            const [path] = params.deep_unpack();
            if (signal === 'JobAdded')
                this._addJob(path).catch(e => logError(e, 'megaftd job'));
            else if (signal === 'JobRemoved')
                this._removeJob(path);
        });
        const [paths] = await call(this._evaluations, 'ListJobs', null);
        for (const path of paths)
            await this._addJob(path);
        this._changed();
    }

    async _addJob(path) {
        if (this._jobs.has(path))
            return;
        const proxy = await newProxy(this._info, path, JOB_IFACE, this._cancellable);
        const ids = [
            proxy.connect('g-properties-changed', () => this._changed()),
            proxy.connect('g-signal', (_p, _sender, signal, params) => {
                if (signal === 'Booked' && this._cb.onBooked) {
                    const [start, end] = params.deep_unpack();
                    this._cb.onBooked(snapshot(path, proxy), Number(start), Number(end));
                }
            }),
        ];
        this._jobs.set(path, { proxy, ids });
        this._changed();
    }

    _removeJob(path) {
        const j = this._jobs.get(path);
        if (!j)
            return;
        for (const id of j.ids)
            j.proxy.disconnect(id);
        this._jobs.delete(path);
        this._changed();
    }

    _vanished() {
        for (const path of [...this._jobs.keys()])
            this._removeJob(path);
        if (this._evaluations && this._evalSignalId)
            this._evaluations.disconnect(this._evalSignalId);
        this._evaluations = null;
        this._evalSignalId = 0;
        if (this._available) {
            this._available = false;
            if (this._cb.onAvailable)
                this._cb.onAvailable(false);
            this._changed();
        }
    }

    destroy() {
        this._cancellable.cancel();
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }
        this._vanished();
        this._cb = {};
    }
};

/** "Tue 09:00–09:30" in local time. */
function formatSpan(start, end) {
    const fmt = (t, f) => GLib.DateTime.new_from_unix_local(t).format(f);
    return `${fmt(start, '%a %H:%M')}–${fmt(end, '%H:%M')}`;
}
/* exported formatSpan */
