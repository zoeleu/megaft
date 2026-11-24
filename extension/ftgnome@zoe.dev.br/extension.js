// -*- mode: js; js-indent-level: 4; indent-tabs-mode: nil -*-
/* ft_gnome — 42 logtime and evaluations in the GNOME top bar.
 *
 * GNOME 42 extension: legacy `imports` modules (ES modules are only
 * supported from GNOME 45), init()/enable()/disable() lifecycle. */
'use strict';

const { Clutter, Gio, GLib, GObject, St } = imports.gi;
const ExtensionUtils = imports.misc.extensionUtils;
const Config = imports.misc.config;
const Main = imports.ui.main;
const PanelMenu = imports.ui.panelMenu;
const PopupMenu = imports.ui.popupMenu;
const ModalDialog = imports.ui.modalDialog;
const Dialog = imports.ui.dialog;

const Me = ExtensionUtils.getCurrentExtension();
const Http = Me.imports.lib.http;
const Secrets = Me.imports.lib.secrets;
const Client = Me.imports.lib.client;
const Megaftd = Me.imports.lib.megaftd;

function openUri(uri) {
    try {
        Gio.AppInfo.launch_default_for_uri(uri, global.create_app_launch_context(0, -1));
    } catch (e) {
        Main.notifyError('ft_gnome', `Could not open ${uri}: ${e.message}`);
    }
}

/** Small "are you sure?" dialog. */
const ConfirmDialog = GObject.registerClass(
class FtConfirmDialog extends ModalDialog.ModalDialog {
    _init(title, body, confirmLabel, onConfirm) {
        super._init({ styleClass: 'ftgnome-dialog' });
        this.contentLayout.add_child(new Dialog.MessageDialogContent({ title, description: body }));
        this.addButton({ label: 'Cancel', action: () => this.close(), key: Clutter.KEY_Escape });
        this.addButton({
            label: confirmLabel,
            action: () => {
                this.close();
                onConfirm();
            },
        });
    }
});

const Indicator = GObject.registerClass(
class FtIndicator extends PanelMenu.Button {
    _init(ext) {
        super._init(0.0, 'ft_gnome');
        this._ext = ext;
        this._destroyed = false;

        const box = new St.BoxLayout({ style_class: 'panel-status-menu-box' });
        box.add_child(new St.Label({ text: '42', y_align: Clutter.ActorAlign.CENTER, style_class: 'ftgnome-header' }));
        this._label = new St.Label({ text: '', y_align: Clutter.ActorAlign.CENTER, style_class: 'ftgnome-label' });
        box.add_child(this._label);
        // megaftd booking jobs ("⏳ codexion 1/2")
        this._jobLabel = new St.Label({ text: '', y_align: Clutter.ActorAlign.CENTER, style_class: 'ftgnome-label' });
        this._jobLabel.visible = false;
        box.add_child(this._jobLabel);
        this.add_child(box);

        // header / status
        this._statusItem = new PopupMenu.PopupMenuItem('', { reactive: false });
        this.menu.addMenuItem(this._statusItem);
        this._logtimeItem = new PopupMenu.PopupMenuItem('', { reactive: false });
        this.menu.addMenuItem(this._logtimeItem);
        this._evalSection = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._evalSection);

        // megaftd booking jobs (only shown when megaftd is around)
        this._jobsSeparator = new PopupMenu.PopupSeparatorMenuItem('Booking jobs');
        this.menu.addMenuItem(this._jobsSeparator);
        this._jobsSection = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._jobsSection);
        this._openAppItem = new PopupMenu.PopupMenuItem('Open evalfinder…');
        this._openAppItem.connect('activate', () => this._ext.openEvalfinder());
        this.menu.addMenuItem(this._openAppItem);
        this.showJobs([], false);

        // linking
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._codeItem = new PopupMenu.PopupMenuItem('', { reactive: true });
        this._codeItem.label.add_style_class_name('ftgnome-code');
        this._codeItem.connect('activate', () => {
            if (this._pendingLink)
                openUri(this._pendingLink.verificationUriComplete);
        });
        this.menu.addMenuItem(this._codeItem);
        this._linkItem = new PopupMenu.PopupMenuItem('Link this computer…');
        this._linkItem.connect('activate', () => this._ext.startLink());
        this.menu.addMenuItem(this._linkItem);
        this._cancelLinkItem = new PopupMenu.PopupMenuItem('Cancel linking');
        this._cancelLinkItem.connect('activate', () => this._ext.cancelLink());
        this.menu.addMenuItem(this._cancelLinkItem);

        // device management
        this._devicesMenu = new PopupMenu.PopupSubMenuMenuItem('Devices');
        this._devicesMenu.menu.connect('open-state-changed', (_m, open) => {
            if (open)
                this._loadDevices();
        });
        this.menu.addMenuItem(this._devicesMenu);
        this._unlinkItem = new PopupMenu.PopupMenuItem('Unlink this computer…');
        this._unlinkItem.connect('activate', () => {
            new ConfirmDialog('Unlink this computer?',
                'It will stop showing your 42 data until you link it again.',
                'Unlink', () => this._ext.unlink()).open();
        });
        this.menu.addMenuItem(this._unlinkItem);

        // misc
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._refreshItem = new PopupMenu.PopupMenuItem('Refresh now');
        this._refreshItem.connect('activate', () => this._ext.refresh());
        this.menu.addMenuItem(this._refreshItem);
        const web = new PopupMenu.PopupMenuItem('Open web dashboard');
        web.connect('activate', () => {
            const base = this._ext.baseUrl();
            if (base)
                openUri(`${base}/devices`);
        });
        this.menu.addMenuItem(web);
        const prefs = new PopupMenu.PopupMenuItem('Settings');
        prefs.connect('activate', () => ExtensionUtils.openPrefs());
        this.menu.addMenuItem(prefs);

        this._pendingLink = null;
        this._summary = null;
        this.showUnlinked();
    }

    /** Mirrors megaftd's jobs: top-bar label for running ones, a menu item per job. */
    showJobs(jobs, available) {
        if (this._destroyed)
            return;
        const running = jobs.filter(j => j.state === 'running');
        this._jobLabel.visible = running.length > 0;
        this._jobLabel.text = running.map(j => `⏳ ${j.project} ${j.booked.length}/${j.count}`).join('  ');
        this._jobsSeparator.visible = available;
        this._openAppItem.visible = available;
        this._jobsSection.removeAll();
        if (!available)
            return;
        if (jobs.length === 0) {
            const none = new PopupMenu.PopupMenuItem('No booking jobs', { reactive: false });
            none.label.add_style_class_name('ftgnome-muted');
            this._jobsSection.addMenuItem(none);
        }
        for (const j of jobs) {
            const verb = j.dryRun ? 'found' : 'booked';
            const title = `${j.project}: ${j.booked.length}/${j.count} ${verb} — ${j.state}`;
            const item = new PopupMenu.PopupMenuItem(title);
            const detail = new St.Label({ text: j.lastLog, style_class: 'ftgnome-muted ftgnome-joblog' });
            item.add_child(detail);
            if (j.state === 'running') {
                item.connect('activate', () => {
                    new ConfirmDialog(`Stop booking ${j.project}?`,
                        `${j.booked.length} of ${j.count} evaluation(s) ${verb} so far. Booked ones stay booked.`,
                        'Stop', () => this._ext.megaftd.cancel(j.path).catch(e =>
                            Main.notifyError('evalfinder', `Could not stop the job: ${e.message}`))).open();
                });
            } else {
                // Finished: clicking clears it from the list.
                item.connect('activate', () => this._ext.megaftd.dismiss(j.path).catch(e =>
                    Main.notifyError('evalfinder', `Could not clear the job: ${e.message}`)));
            }
            this._jobsSection.addMenuItem(item);
        }
    }

    _setStatus(text, error = false) {
        this._statusItem.label.text = text;
        if (error)
            this._statusItem.label.add_style_class_name('ftgnome-error');
        else
            this._statusItem.label.remove_style_class_name('ftgnome-error');
    }

    _setLinkedUi(linked) {
        this._linkItem.visible = !linked && !this._pendingLink;
        this._codeItem.visible = !!this._pendingLink;
        this._cancelLinkItem.visible = !!this._pendingLink;
        this._devicesMenu.visible = linked;
        this._unlinkItem.visible = linked;
        this._refreshItem.visible = linked;
        this._logtimeItem.visible = linked && !!this._summary;
    }

    showUnlinked(message) {
        this._summary = null;
        this._label.text = '';
        this._evalSection.removeAll();
        this._setStatus(message || 'Not linked: link this computer to your 42 account.', !!message);
        this._setLinkedUi(false);
    }

    showLinking(link) {
        this._pendingLink = link;
        this._codeItem.label.text = `Code: ${link.userCode}  (click to open)`;
        this._setStatus('Approve this code in your browser…');
        this._setLinkedUi(false);
    }

    clearLinking() {
        this._pendingLink = null;
        this._setLinkedUi(this._ext.isLinked());
    }

    showError(message) {
        this._setStatus(message, true);
        this._setLinkedUi(this._ext.isLinked());
    }

    showSummary(s) {
        this._summary = s;
        const settings = this._ext.settings;
        const lt = Client.logtimeTotals(s.logtime);
        const next = (s.evaluations || [])[0];

        const parts = [];
        if (settings.get_boolean('show-logtime'))
            parts.push(Client.formatDuration(lt.today));
        if (settings.get_boolean('show-week'))
            parts.push(`wk ${Client.formatDuration(lt.week)}`);
        if (settings.get_boolean('show-next-eval') && next)
            parts.push(`✎ ${Client.formatWhen(next.begin_at)}`);
        this._label.text = parts.join(' · ');

        const u = s.user;
        const extra = [u.wallet !== null ? `${u.wallet} ₳` : null,
            u.correction_points !== null ? `${u.correction_points} ev.pts` : null,
            u.location ? `@ ${u.location}` : null].filter(Boolean).join(' · ');
        this._setStatus(`${u.login}${extra ? `  —  ${extra}` : ''}`);
        this._logtimeItem.label.text = `Logtime: today ${Client.formatDuration(lt.today)} · this week ${Client.formatDuration(lt.week)}`;

        this._evalSection.removeAll();
        const evals = s.evaluations || [];
        if (evals.length === 0) {
            this._evalSection.addMenuItem(new PopupMenu.PopupMenuItem('No upcoming evaluations', { reactive: false }));
        } else {
            for (const e of evals.slice(0, 6)) {
                const who = e.with && e.with.length ? e.with.join(', ') : 'hidden';
                const what = e.role === 'corrector' ? `You evaluate ${who}` : `Evaluated by ${who}`;
                this._evalSection.addMenuItem(new PopupMenu.PopupMenuItem(
                    `${Client.formatWhen(e.begin_at)}  ${e.project || '?'} — ${what}`, { reactive: false }));
            }
        }
        this._setLinkedUi(true);
    }

    async _loadDevices() {
        const sub = this._devicesMenu.menu;
        sub.removeAll();
        sub.addMenuItem(new PopupMenu.PopupMenuItem('Loading…', { reactive: false }));
        let devices;
        try {
            devices = await this._ext.client.devices();
        } catch (e) {
            if (this._destroyed)
                return;
            sub.removeAll();
            sub.addMenuItem(new PopupMenu.PopupMenuItem(`Error: ${e.message}`, { reactive: false }));
            return;
        }
        if (this._destroyed)
            return;
        sub.removeAll();
        for (const d of devices) {
            const seen = d.last_seen_at ? Client.formatWhen(d.last_seen_at) : 'never';
            const item = new PopupMenu.PopupMenuItem(
                `${d.current ? '● ' : ''}${d.name}${d.current ? ' (this computer)' : ''} — seen ${seen}`);
            item.connect('activate', () => {
                const body = d.current
                    ? 'This computer will be unlinked immediately.'
                    : `“${d.name}” will lose access immediately.`;
                new ConfirmDialog(`Revoke “${d.name}”?`, body, 'Revoke', () => this._ext.revokeDevice(d)).open();
            });
            sub.addMenuItem(item);
        }
        const manage = new PopupMenu.PopupMenuItem('Manage on the web…');
        manage.connect('activate', () => {
            const base = this._ext.baseUrl();
            if (base)
                openUri(`${base}/devices`);
        });
        sub.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        sub.addMenuItem(manage);
    }

    destroy() {
        this._destroyed = true;
        super.destroy();
    }
});

class Extension {
    enable() {
        this.settings = ExtensionUtils.getSettings();
        this._http = new Http.Http(`ft_gnome/${Me.metadata.version} (GNOME Shell ${Config.PACKAGE_VERSION})`);
        this.client = new Client.FtClient({
            http: this._http,
            store: Secrets,
            getBaseUrl: () => this.baseUrl(),
            deviceName: () => this.settings.get_string('device-name').trim() || GLib.get_host_name(),
            platform: `GNOME Shell ${Config.PACKAGE_VERSION}`,
        });
        this.client.onUnlinked = () => {
            this._indicator.showUnlinked('This computer was revoked. Link it again to continue.');
            Main.notify('ft_gnome', 'This computer was unlinked from your 42 account.');
        };

        this._indicator = new Indicator(this);
        Main.panel.addToStatusArea(Me.uuid, this._indicator);

        // megaftd: booking jobs. Never starts the service; just follows it when it's running.
        try {
            this.megaftd = new Megaftd.Watcher(`${Me.path}/dbus/megaft1.xml`, {
                onChange: jobs => this._indicator && this._indicator.showJobs(jobs, this.megaftd.available),
                onAvailable: up => this._indicator && this._indicator.showJobs(up ? this.megaftd.jobs() : [], up),
                onBooked: (job, start, end) => {
                    const what = job.dryRun ? 'Found (dry run)' : 'Booked';
                    Main.notify(`evalfinder: ${what} ${job.project}`, Megaftd.formatSpan(start, end));
                },
            });
        } catch (e) {
            logError(e, 'ftgnome: megaftd watcher');
            this.megaftd = null;
        }

        this._settingsIds = [
            this.settings.connect('changed::refresh-interval', () => this._schedule()),
            this.settings.connect('changed::server-url', () => this._reload()),
            ...['show-logtime', 'show-week', 'show-next-eval'].map(k =>
                this.settings.connect(`changed::${k}`, () => {
                    if (this._indicator && this._indicator._summary)
                        this._indicator.showSummary(this._indicator._summary);
                })),
        ];
        this._reload();
    }

    disable() {
        if (this._timerId) {
            GLib.source_remove(this._timerId);
            this._timerId = 0;
        }
        if (this.megaftd) {
            this.megaftd.destroy();
            this.megaftd = null;
        }
        for (const id of this._settingsIds || [])
            this.settings.disconnect(id);
        this._settingsIds = null;
        if (this.client)
            this.client.destroy();
        if (this._http)
            this._http.destroy();
        if (this._indicator)
            this._indicator.destroy();
        this._indicator = null;
        this.client = null;
        this._http = null;
        this.settings = null;
    }

    openEvalfinder() {
        const app = Gio.DesktopAppInfo.new('evalfinder.desktop');
        try {
            if (app)
                app.launch([], null);
            else
                GLib.spawn_command_line_async('evalfinder-gui');
        } catch (e) {
            Main.notifyError('evalfinder', `Could not start evalfinder: ${e.message}`);
        }
    }

    baseUrl() {
        return this.settings ? Http.normalizeBaseUrl(this.settings.get_string('server-url')) : null;
    }

    isLinked() {
        return !!this.client && this.client.isLinked();
    }

    _alive(client) {
        return this.client === client && this._indicator;
    }

    async _reload() {
        const client = this.client;
        client.cancelLink();
        this._indicator.clearLinking();
        if (!this.baseUrl()) {
            this._indicator.showUnlinked('Invalid server URL (must be https://, or http://localhost). Check Settings.');
            return;
        }
        try {
            await client.load();
        } catch (e) {
            if (this._alive(client))
                this._indicator.showError(`Keyring error: ${e.message}`);
            return;
        }
        if (!this._alive(client))
            return;
        if (client.isLinked())
            this.refresh();
        else
            this._indicator.showUnlinked();
        this._schedule();
    }

    _schedule() {
        if (this._timerId)
            GLib.source_remove(this._timerId);
        this._timerId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT,
            this.settings.get_uint('refresh-interval'), () => {
                this.refresh();
                return GLib.SOURCE_CONTINUE;
            });
    }

    async refresh() {
        const client = this.client;
        if (!client || !client.isLinked())
            return;
        try {
            const summary = await client.summary();
            if (this._alive(client))
                this._indicator.showSummary(summary);
        } catch (e) {
            if (!this._alive(client))
                return;
            if (e instanceof Client.NotLinkedError)
                this._indicator.showUnlinked(e.message);
            else if (e.code === 'intra_reauth_required')
                this._indicator.showError('42 access expired: sign in again on the web dashboard.');
            else
                this._indicator.showError(`Could not refresh: ${e.message}`);
        }
    }

    async startLink() {
        const client = this.client;
        let link;
        try {
            link = await client.startLink();
        } catch (e) {
            if (this._alive(client))
                this._indicator.showError(`Could not start linking: ${e.message}`);
            return;
        }
        if (!this._alive(client))
            return;
        this._indicator.showLinking(link);
        Main.notify('ft_gnome: link this computer', `Confirm the code ${link.userCode} in your browser.`);
        openUri(link.verificationUriComplete);
        try {
            const ok = await link.wait();
            if (!this._alive(client) || !ok)
                return;
            this._indicator.clearLinking();
            Main.notify('ft_gnome', 'This computer is now linked.');
            this.refresh();
        } catch (e) {
            if (!this._alive(client))
                return;
            this._indicator.clearLinking();
            this._indicator.showUnlinked(e.message);
        }
    }

    cancelLink() {
        this.client.cancelLink();
        this._indicator.clearLinking();
        this._indicator.showUnlinked();
    }

    async unlink() {
        const client = this.client;
        await client.unlink();
        if (this._alive(client))
            this._indicator.showUnlinked();
    }

    async revokeDevice(device) {
        const client = this.client;
        if (device.current) {
            await this.unlink();
            return;
        }
        try {
            await client.revokeDevice(device.id);
            Main.notify('ft_gnome', `“${device.name}” was revoked.`);
        } catch (e) {
            if (this._alive(client))
                Main.notifyError('ft_gnome', `Could not revoke “${device.name}”: ${e.message}`);
        }
    }
}

/* exported init */
function init() {
    return new Extension();
}
