# Plugins (new, in progress)

> Plugins are new and will change. Nobody reviews them: install only plugins from people you trust. A plugin's
> browser extensions run inside your profiles and can read the sites they are allowed on.

A plugin adds things to the profiles of the groups it is switched on for (Plugins page -> *Switched on for*):

| It can add | Where it shows |
|---|---|
| **fields** (text, secret, note, select) | the profile editor, under General; the values are stored in the profile, so team sync carries them |
| **start pages** | opened with the profile's own start pages |
| **bookmarks** | on the bookmarks bar, after the group's |
| **extensions** (unpacked Chrome extensions inside the plugin) | loaded into the browser |

Plugins cannot run programs yet; actions and an AI assistant are the next steps.

## Installing

Plugins page -> paste a GitHub link and press **Get plugin**: you see what it adds before you install it.

* `https://github.com/owner/repo` - the plugin is at the top of the repository;
* `https://github.com/owner/repo/tree/main/some/folder` - the plugin is in a folder of a bigger repository;
* a folder, `.zip` or `.tar.gz` on this computer.

A GitHub plugin is pinned to the commit it was installed from. **Check for update** looks for a newer commit, shows
the page with every change, and **Update** installs it only when you press it. From a terminal:

```
citro plugin add https://github.com/afftroen/Citroenition/tree/main/examples/plugins/tiktok-pack
citro plugin ls
citro plugin update tiktok-pack --check
citro plugin rm tiktok-pack
```

## Writing one

A plugin is a folder with a `citroenition-plugin.json`. A complete example is
[examples/plugins/tiktok-pack](../examples/plugins/tiktok-pack/citroenition-plugin.json):

```json
{
  "id": "tiktok-pack",
  "name": "TikTok pack",
  "version": "0.1.0",
  "author": "you",
  "description": "What it is for, in one line.",
  "fields": [
    { "key": "email", "label": "TikTok e-mail" },
    { "key": "twofa", "label": "2FA secret", "type": "secret", "hint": "Shown under the field" },
    { "key": "status", "label": "Status", "type": "select", "options": ["new", "active", "banned"] },
    { "key": "notes", "label": "Notes", "type": "note" }
  ],
  "start_pages": ["https://ads.tiktok.com/i18n/home"],
  "bookmarks": [{ "name": "Ads Manager", "url": "https://ads.tiktok.com/i18n/home" }],
  "extensions": ["my-extension"]
}
```

* `id`: 1-40 lowercase letters, digits or `-`; it is also the folder name, so keep it once published.
* `fields[].key`: letters, digits or `_`, unique in the plugin. `type`: `text` (default), `secret`, `note`, `select`.
* `start_pages`, bookmark `url`: `http://` or `https://` addresses.
* `extensions`: folders inside the plugin, each with the extension's `manifest.json`.

Citroenition says exactly what is wrong with a manifest when it cannot install it. Put the folder in a public GitHub
repository and share the link.
