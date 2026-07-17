# persona-wire-adapter-apple-notes 0.14.0

persona-wire Adapter for the local Notes.app database (scheme `applenotes://`).

## Architecture

`AppleNotesAdapter` is a stateless [`Adapter`] impl. `Notes.app`'s
`NoteStore.sqlite` only exists on macOS, so the crate splits into:

- [`parse_apple_notes_uri`] — `WireUri` → `AppleNotesUriSpec` (folder
  filter + title substring query + row limit). Platform-independent, so
  it is unit-tested on every target.
- The `macos` module (behind `#[cfg(target_os = "macos")]`) — resolves
  `NoteStore.sqlite`'s on-disk path and runs the read-only filter query
  via `rusqlite`, including the Core Data → RFC3339 timestamp
  conversion.

On non-macOS targets `fetch` returns `WireError::Storage("apple-notes
adapter: unsupported platform (macOS only)")` without touching the
filesystem, so the crate still compiles workspace-wide.

## URI grammar

```text
applenotes://[folder]/?query=<substring>&limit=N
```

- `folder` (the URI host) is optional; an absent or empty host means
  "all folders".
- `?query=<substring>` is a case-insensitive substring match against
  note titles (absent = all notes).
- `?limit=N` caps the number of items returned (default
  [`DEFAULT_LIMIT`]). A non-numeric or zero value fails loud.
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-rss` / `-obsidian`).

## Output shape

```json
{
  "folder": "<folder name>|null",
  "query":  "<query substring>|null",
  "notes": [
    { "id": "<Core Data primary key as string>", "title": "...|null",
      "folder": "...|null", "created": "<RFC3339>|null",
      "modified": "<RFC3339>|null" }
  ]
}
```

`notes` is ordered by `modified` descending (newest first), capped at
`limit`. Note bodies are out of MVP scope — Notes.app stores them as
gzip'd protobuf in `ZICNOTEDATA` — and may be added later via an
AppleScript fallback.

