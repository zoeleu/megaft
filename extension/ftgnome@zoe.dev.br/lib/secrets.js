// -*- mode: js; js-indent-level: 4; indent-tabs-mode: nil -*-
/* Device credentials live in the GNOME keyring (libsecret), keyed by server
 * URL, so they are encrypted at rest and never written to GSettings/dconf.
 * If libsecret is unavailable, falls back to a file in a 0700 directory. */
'use strict';

const { GLib, Gio } = imports.gi;

let Secret = null;
try {
    imports.gi.versions.Secret = '1';
    Secret = imports.gi.Secret;
} catch (e) {
    log(`[ftgnome] libsecret unavailable, using file storage: ${e.message}`);
}

const SCHEMA = Secret
    ? new Secret.Schema('dev.zoe.ftgnome.DeviceCredentials', Secret.SchemaFlags.NONE, {
        server: Secret.SchemaAttributeType.STRING,
    })
    : null;

function fallbackPath(server) {
    const dir = GLib.build_filenamev([GLib.get_user_config_dir(), 'ftgnome']);
    GLib.mkdir_with_parents(dir, 0o700);
    const name = GLib.compute_checksum_for_string(GLib.ChecksumType.SHA256, server, -1);
    return GLib.build_filenamev([dir, `${name}.json`]);
}

/** @returns {Promise<object|null>} */
function load(server) {
    if (!Secret) {
        try {
            const [ok, contents] = GLib.file_get_contents(fallbackPath(server));
            return Promise.resolve(ok ? JSON.parse(new TextDecoder().decode(contents)) : null);
        } catch (e) {
            return Promise.resolve(null);
        }
    }
    return new Promise((resolve, reject) => {
        Secret.password_lookup(SCHEMA, { server }, null, (_src, res) => {
            try {
                const value = Secret.password_lookup_finish(res);
                resolve(value ? JSON.parse(value) : null);
            } catch (e) {
                reject(e);
            }
        });
    });
}

/** @returns {Promise<void>} */
function save(server, creds) {
    const value = JSON.stringify(creds);
    if (!Secret) {
        const file = Gio.File.new_for_path(fallbackPath(server));
        file.replace_contents(new TextEncoder().encode(value), null, false,
            Gio.FileCreateFlags.PRIVATE | Gio.FileCreateFlags.REPLACE_DESTINATION, null);
        return Promise.resolve();
    }
    return new Promise((resolve, reject) => {
        Secret.password_store(SCHEMA, { server }, Secret.COLLECTION_DEFAULT,
            `ft_gnome device credentials (${server})`, value, null, (_src, res) => {
                try {
                    Secret.password_store_finish(res);
                    resolve();
                } catch (e) {
                    reject(e);
                }
            });
    });
}

/** @returns {Promise<void>} */
function clear(server) {
    if (!Secret) {
        try {
            Gio.File.new_for_path(fallbackPath(server)).delete(null);
        } catch (e) {
            // already gone
        }
        return Promise.resolve();
    }
    return new Promise(resolve => {
        Secret.password_clear(SCHEMA, { server }, null, (_src, res) => {
            try {
                Secret.password_clear_finish(res);
            } catch (e) {
                log(`[ftgnome] could not clear credentials: ${e.message}`);
            }
            resolve();
        });
    });
}

var backend = Secret ? 'libsecret' : 'file';
