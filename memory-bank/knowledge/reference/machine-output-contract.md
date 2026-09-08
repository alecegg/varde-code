# Machine-output contract

## Scope

Every machine-readable `varde-code` command returns one JSON object. This
includes query modes, `scan`, `test`, maintenance commands, and watcher
management. `nav_map --format text` is the sole text-mode exception.

## Envelope

```json
{
  "ok": true,
  "data": { "...": "command payload" },
  "meta": { "compact": true, "truncated": false }
}
```

`ok` indicates command success. `data` holds its result.
`meta.compact` reports the active compact policy. `meta.truncated` reports a
declared omission.

Failures retain this shape:

```json
{
  "ok": false,
  "data": {
    "error": {
      "code": "invalid_input",
      "message": "input is not valid JSON: ..."
    }
  },
  "meta": { "compact": false, "truncated": false }
}
```

Callers branch on `ok`. Then inspect `data.error.code`.
The message provides user-facing context. Query errors use the envelope and
remain process-successful. `scan` and `test` return nonzero for tool errors or
CI-gate conditions. They print an envelope first. Panics use stderr and a
nonzero status.

## Compact payload policy

Commands with `repoRoot` or `rulesDir` emit known repository paths as relative
by default. This avoids repeated absolute prefixes. Set `absolutePaths: true`
to retain them. External paths, source text, and `dbPath`-only calls stay
unchanged. Relative file paths round-trip into query modes.

Spans carry `start_line` and `end_line` by default. Set
`includeSpanDetail: true` to also receive `start_byte`, `end_byte`,
`start_col`, and `end_col`.

Dense outputs prefer summaries. `build` reports `changedFilesCount` and
`changedFilesSample` by default. `--changed-files` returns every path.
`nav_map` applies a per-section token budget. Its flows can report
`childrenOmitted` instead of repeating a large call tree.

## Truncation

Omission is explicit. A truncated `nav_map` includes
`data.guide.truncated` entries shaped as:

```json
{
  "section": { "shown": 12, "total": 54, "more": "follow-up query" }
}
```

`shown` and `total` quantify the omission. `more` names the recovery route.
`meta.truncated` is true when this guide is populated. Consumers preserve
these fields when forwarding partial results.

## Text exception

`nav_map --format json` is canonical machine output. `nav_map --format text`
renders the same result for people. It can use headings and text guidance.
Tools must use JSON when parsing or composing results.
