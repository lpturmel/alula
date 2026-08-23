# Contributing a theme

Theme pull requests must include:

1. A stable lowercase ID containing only letters, numbers, and hyphens.
2. A complete `themes/<id>.toml` accepted by Alula's current `AppConfig` schema.
3. One `[[themes]]` entry in `registry.toml` with HTTPS source and license URLs.
4. A palette preview matching the theme's background, foreground, accent, and
   primary colors exactly.
5. An entry in `THIRD_PARTY_NOTICES.md` containing the upstream copyright and
   redistribution license.

Only data-only themes under licenses that permit redistribution are accepted.
Do not submit proprietary themes, executable code, fonts, icons, images, or
network-loaded assets. Prefer canonical palettes from the theme author's
official repository over unofficial ports.

Run `taplo lint` before opening a pull request. Alula's own test suite also
loads every theme through the production parser and verifies registry IDs,
paths, modes, previews, and source metadata.
