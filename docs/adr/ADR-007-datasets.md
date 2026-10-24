# ADR-007 — Datasets

Status: **accepted** · 16 Oct 2026

## Context

Today every iteration sends the same thing: a user from `secrets`, IDs from `vars` or from `{{$randomInt}}`. Against a real API that isn't enough. If everyone logs in as the same user, you measure the cache of a single session, you hit the locks of the same row, and it doesn't look like real traffic. ADR-002 reserved the `data` field for this.

## Decision

### Format

```yaml
data:
  users: { file: data/users.csv }                     # pick: sequential (default)
  products: { file: data/products.json, pick: random }

flows:
  main:
    steps:
      - name: login
        request:
          method: POST
          url: /auth/login
          json: { user: "{{users.user}}", password: "{{users.password}}" }
      - name: get_product
        request: { method: GET, url: "/products/{{products.id}}" }
```

- `file` is relative to `.crabfall/`. Two formats are accepted:
  - **`.csv` with a header.** Every cell is a string: inferring types breaks values like `01234`.
  - **`.json` with an array of objects.** It keeps the types. The columns are the keys of the first object, and every row has to have exactly those keys.
- Column and dataset names use letters, digits and `_`. A dataset can't be called `env`, because `{{env.X}}` are already the secrets.
- `pick`:
  - `sequential`: rows in order, shared among all VUs. At the end it starts over.
  - `random`: a random row on each iteration.

### One row per iteration

Each iteration takes **one** row from every dataset its flow uses and sees it in all its steps as `{{dataset.column}}`. That way the email and the password always come from the same row. A flow that doesn't use a dataset doesn't consume rows, so the sequence doesn't skip rows because of other flows. `crabfall check` uses the first row with `sequential`.

The `{{dataset.column}}` syntax doesn't collide with anything: variable and `extract` names have no dots, and `env.` is already taken by the secrets. With a value that is exactly `"{{users.id}}"`, the type is kept as with any variable (ADR-002 §6).

### Not used in `setup`

`setup` runs once, so a row there would make no sense. If `setup` uses a column, it's an error that explains this. One login per user and per VU is something else ("per-VU setup") and stays in the backlog: today it's done with the login as the first step of the flow, as in the example.

### The approval signs the content

Changing the file changes the traffic: other users, other IDs. That's why the canonical plan (ADR-004) includes, for each dataset, `name`, `file`, `pick` and the **file's SHA-256**, but not its content, which can be large. If the CSV is edited, `crabfall run` rejects the approval just as if the scenario had been edited. The `data` key only appears in the canonical plan when there are datasets, so approvals of scenarios that don't use them aren't invalidated.

The `crabfall approve` summary and the `crabfall run` header show each dataset with its file, row count and `pick`.

### Errors

`crabfall validate` (with a project), `crabfall check`, `crabfall approve` and `crabfall run` read the files and report:

- a missing file, one with no rows, with an extension other than `.csv` or `.json`, or not UTF-8;
- repeated or invalid column names, and rows with more or fewer cells (with the row number);
- `{{users.pass}}` when `users` has no such column, listing the ones it has;
- `{{x.y}}` when there is no dataset `x`;
- columns used in `setup`.

## Rejected alternatives

- **Loose columns (`{{email}}`), like Artillery:** two datasets with the same column collide, and the YAML doesn't show where each value comes from.
- **Inferring types in CSV:** `01234` would become `1234` and `true` a boolean without anyone asking. For types, JSON.
- **`pick: unique`** (each row only once, stopping when they run out): it forces deciding what happens to iterations without a row, and how they're counted. To be added when a real case asks for it, for example registering new users.
- **Signing the whole content:** the hash is enough to detect the change. To see *what* changed in the file, there's git.

## Consequences

- **For:** scenarios with real users and IDs without logic in the YAML. The signature covers the data. Errors name the column and the row.
- **Against:** the whole file is loaded into memory (fine up to tens of MB). Test data usually has test users' credentials: if they shouldn't be versioned, the file has to go in the project's `.gitignore`. `crabfall` doesn't decide that.

## `pick: unique` (20 Oct 2026)

- **What it does:** each row is used only once in the whole run, for example to register users or redeem coupons. The counter belongs to the plan: all VUs share it.
- **When they run out:** the row is taken when the iteration is dispatched, before it counts as started. If none are left, the load ends in order (no more iterations start and the ones in progress are awaited), with `data_exhausted` (dataset and second) in `results.json` and a warning at the top. It doesn't count as a failure: it's the natural end of "one row per iteration". It isn't a stop by the person either (`stopped_at_s` stays empty).
- **In `vu_setup`:** each VU takes a different row, so the plan requires at least one per VU; otherwise it isn't built.
- **For whoever approves:** if the dataset has fewer rows than the planned iterations, the summary says so ("unique, 50 rows for ≈600 iterations: the load ends when they run out").
