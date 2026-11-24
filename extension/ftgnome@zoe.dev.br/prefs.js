// -*- mode: js; js-indent-level: 4; indent-tabs-mode: nil -*-
/* Preferences for GNOME 42 (GTK 4 + libadwaita 1.1: only widgets that exist
 * in 1.1 are used, e.g. ActionRow + Gtk.Entry instead of Adw.EntryRow).
 * Holds no credentials: linking and device management live in the panel
 * menu and the web dashboard. */
'use strict';

const { Adw, Gio, GLib, Gtk } = imports.gi;
const ExtensionUtils = imports.misc.extensionUtils;
const Me = ExtensionUtils.getCurrentExtension();
const Http = Me.imports.lib.http;

/* exported init fillPreferencesWindow */
function init() {
}

function switchRow(settings, key, title, subtitle) {
    const row = new Adw.ActionRow({ title, subtitle });
    const sw = new Gtk.Switch({ valign: Gtk.Align.CENTER });
    settings.bind(key, sw, 'active', Gio.SettingsBindFlags.DEFAULT);
    row.add_suffix(sw);
    row.activatable_widget = sw;
    return row;
}

function entryRow(settings, key, title, subtitle, placeholder, validate) {
    const row = new Adw.ActionRow({ title, subtitle });
    const entry = new Gtk.Entry({
        valign: Gtk.Align.CENTER,
        hexpand: true,
        width_chars: 28,
        placeholder_text: placeholder || '',
        text: settings.get_string(key),
    });
    const apply = () => {
        const v = entry.text.trim();
        if (validate && !validate(v)) {
            entry.add_css_class('error');
            return;
        }
        entry.remove_css_class('error');
        if (settings.get_string(key) !== v)
            settings.set_string(key, v);
    };
    // apply on Enter / focus-out rather than per keystroke (each change reloads the extension state)
    entry.connect('activate', apply);
    const focus = new Gtk.EventControllerFocus();
    focus.connect('leave', apply);
    entry.add_controller(focus);
    row.add_suffix(entry);
    return row;
}

function fillPreferencesWindow(window) {
    const settings = ExtensionUtils.getSettings();
    window.set_default_size(620, 560);

    const page = new Adw.PreferencesPage({ title: 'General', icon_name: 'preferences-system-symbolic' });

    const server = new Adw.PreferencesGroup({
        title: 'Account',
        description: 'Link this computer and manage your devices from the “42” menu in the top bar.',
    });
    server.add(entryRow(settings, 'server-url', 'Server URL',
        'https:// required (http only for localhost). Changing it unlinks this computer.',
        'https://ftgnome.example.com', v => Http.normalizeBaseUrl(v) !== null));
    server.add(entryRow(settings, 'device-name', 'Device name',
        'Shown in your device list. Applies the next time you link.', GLib.get_host_name()));
    const webRow = new Adw.ActionRow({ title: 'Web dashboard', subtitle: 'See and revoke linked devices' });
    const webBtn = new Gtk.Button({ label: 'Open', valign: Gtk.Align.CENTER });
    webBtn.connect('clicked', () => {
        const base = Http.normalizeBaseUrl(settings.get_string('server-url'));
        if (base)
            Gtk.show_uri(window, `${base}/devices`, 0);
    });
    webRow.add_suffix(webBtn);
    server.add(webRow);
    page.add(server);

    const panel = new Adw.PreferencesGroup({ title: 'Top bar' });
    panel.add(switchRow(settings, 'show-logtime', 'Today’s logtime', null));
    panel.add(switchRow(settings, 'show-week', 'This week’s logtime', 'Since Monday'));
    panel.add(switchRow(settings, 'show-next-eval', 'Next evaluation', 'Time of your next evaluation'));

    const intervalRow = new Adw.ActionRow({ title: 'Refresh interval', subtitle: 'Seconds between updates' });
    const spin = Gtk.SpinButton.new_with_range(60, 3600, 30);
    spin.valign = Gtk.Align.CENTER;
    settings.bind('refresh-interval', spin, 'value', Gio.SettingsBindFlags.DEFAULT);
    intervalRow.add_suffix(spin);
    panel.add(intervalRow);
    page.add(panel);

    window.add(page);
}
