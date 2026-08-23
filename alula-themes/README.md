# Alula Community Themes

A data-only registry of community color themes for the Alula HTTP client.

Every theme is a complete Alula TOML theme, contains no executable code, and
is derived from a publicly documented palette whose license allows
redistribution. Alula validates the same files before previewing or installing
them.

## Layout

```text
registry.toml             Registry metadata and palette previews
themes/*.toml             Installable Alula themes
schemas/*.schema.json     TOML/JSON schemas for editors and validation tools
THIRD_PARTY_NOTICES.md    Upstream attribution and license notices
```

## Consuming the registry

Fetch `registry.toml`, select an entry, then fetch the relative `file`. Clients
must reject absolute paths, parent-directory traversal, unknown schema
versions, invalid colors, and theme files whose name or mode does not match the
registry entry.

The palette preview in the registry is intentionally small so clients can
render a theme gallery without downloading every theme.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Themes are community conversions and
are not endorsed by or affiliated with their upstream projects.

## License

Registry code and documentation are MIT licensed. Individual converted themes
retain their upstream attribution and license notices as documented in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
