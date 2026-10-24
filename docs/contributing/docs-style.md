# Documentation style

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

How the public documentation under `docs/` is organised and kept honest.
Planning documents, audits, research and backlogs are not documentation; they
live under `docs/internal/` and are never published.

## The tree

Pages are grouped by what the reader is doing, not by which crate implements
the feature:

| Folder | Kind | The reader wants to… |
|---|---|---|
| `getting-started/` | tutorials | get something running, in order, once |
| `guides/` | how-to | achieve a goal in a system they already run |
| `clients/` | how-to + reference | connect something to a worker |
| `extending/` | how-to | add a provider, component or transport |
| `reference/` | reference | look up a flag, key, event, endpoint |
| `explanation/` | explanation | understand why it is built this way |
| `contributing/` | how-to | change the project itself |

A page belongs in exactly one folder. If it explains *and* instructs, split
it. A shipped design decision becomes a short entry under
`explanation/decisions/`; the plan it came from stays internal.

## Front matter

Every page starts with a title and three lines:

```markdown
# Page title

**Status:** active | draft
**Last verified:** 2026-09-06
**Owner:** Docs
```

`Last verified` is the date someone last checked the page against the code.
Bump it when you check, not when you edit. `scripts/docs-check.ps1` fails on
a public page without a status line.

## Provider pages

One page per provider under `guides/providers/{stt,tts,llm,s2s}/`, always in
this order so readers can compare pages by position:

1. **What it is** — one paragraph: vendor, model family, local or cloud.
2. **Cargo feature** — the flag the binary needs.
3. **Config** — a complete, copy-pasteable TOML block.
4. **Environment** — API keys and credential variables.
5. **Capabilities** — streaming, word timestamps, end-of-turn, pooling, sample rate.
6. **Limitations** — measured, not assumed; cold-start cost, real-time factor, quirks.
7. **Upstream** — links to the vendor's docs and model card.

Measured numbers cite the setup (device, attention implementation, steps) and
distinguish cold from warm. When you fix a gotcha in code, note it on the
page so a later change does not quietly undo it.

## Generated pages

`scripts/gen-reference.py` renders the enumerable parts of the reference
from the code: the synopsis block in `reference/cli.md` (from `--help`),
the feature table in `reference/cargo-features.md` (from `Cargo.toml`) and
the whole of `reference/providers.md` (from the wizard descriptor tables).
Generated content sits between `<!-- generated:start … -->` markers; edit
the prose around them, never inside. The same script's `--check` mode runs
in CI and fails when a block is stale or when a hand-written page has
drifted: every wizard provider id must appear in the providers guide, every
`PipelineEvent` variant in `reference/events.md`, every feature key in
`reference/cargo-features.md`, every `PipelineConfig` field in
`reference/config.md`.

```sh
cargo build -p voice-pipes-cli
python scripts/gen-reference.py          # regenerate
python scripts/gen-reference.py --check  # what CI runs
```

The site is built by mdBook from `docs/SUMMARY.md`; a new page must be
added there or `docs-check` fails.

## Links and paths

- Link to other pages with relative paths. `docs-check` fails on a broken
  `.md` link, and on a `page.md#anchor` link whose anchor matches no heading
  of that page (slugged the way GitHub does it; `#L24` line references are
  exempt), and warns on a code pointer that no longer resolves.
- Never link from a public page into `docs/internal/`. The assembled public
  tree does not contain it.
- Name a file, function or flag in prose only when the reader has to go
  there. Commands, snippets and error text go in fenced blocks.

## Checks

```powershell
pwsh scripts/docs-check.ps1        # links, front matter, plan index, drift
bash scripts/leak-check.sh         # private names, personal paths, account ids
```

Both run in CI on every push that touches a markdown file.
