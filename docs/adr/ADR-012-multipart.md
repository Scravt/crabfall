# ADR-012 — Multipart bodies (file uploads)

Status: **accepted** · 18 Oct 2026

## Context

Until now, a step could send JSON (`json`) or text (`body`). Uploading files, which is common in real APIs (avatars, attachments, imports), needs `multipart/form-data`. `market-api` has `POST /files` and there was no way to load it.

## Decision

### `multipart`

```yaml
request:
  method: POST
  url: /files
  multipart:
    file: { file: files/attachment.bin, filename: "attachment-{{$uuid}}.bin", content_type: application/octet-stream }
    description: "uploaded by {{users.email}}"
```

- **Fields:** each key is a field name. A string is a text field and accepts `{{variables}}`.
- **Files:** an object with `file`, taken relative to `.crabfall/` like datasets, without absolute paths or `..`.
  - `filename` is optional, defaults to the file's name, and accepts variables.
  - `content_type` is optional and defaults to the extension's: the common types, and otherwise `application/octet-stream`.
- **Exclusions:** it excludes `json` and `body`. Crabfall sets the request's `content-type`, because it carries the boundary, so setting it by hand is a validation error.
- **Format:** the body follows RFC 7578. In names, quotes and line breaks are encoded as browsers do (`%22`, `%0D`, `%0A`).

### The files

- **Reading:** they are read once, when building the plan, and sent whole on each request. They aren't read from disk on every iteration, so the generator's disk stays out of the measurement.
- **Size:** up to 100 MB per file, because they live in memory and are copied when building each body.
- **Boundary:** random and fixed per step, and checked not to appear inside any file.
- **What was ruled out:** streaming from disk, because it complicates the client and isn't needed with files this size. Also a `file` with variables (`files/{{x}}.jpg`), because it would mean loading and signing an open set of files. If the file needs to vary, use several steps or several scenarios.

### Signature and summaries

- **Signature:** the canonical plan includes, per field, the text (its template) or the file, with its path, its SHA-256, the `filename` and the `content_type`. Changing the file's content invalidates the approval, as with datasets (ADR-007). The boundary isn't signed because it isn't in the scenario. Scenarios without `multipart` sign as before.
- **Summaries:** `approve` shows each file with its size and the step that uploads it (`Files  files/attachment.bin (1.0 MB) in upload_file`), and so does the `run` header. Whoever approves sees how many bytes each request will send.

## Consequences

- **For:** upload endpoints can be loaded with real files, and checks can compare the `sha256` the server returns with the file's.
- **Against:** with large files, the generator's bandwidth can be the limit. Since ADR-013 the results show bytes per second, and since 20 Oct 2026 `bandwidth_limit` diagnoses it (ADR-005).
