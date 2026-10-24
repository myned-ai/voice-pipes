# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** — do not open a public
GitHub issue.

- Email: **goran@myned.ai** with subject line `[SECURITY] voice-pipes: <summary>`
- Include: affected component (engine / admin console / launcher / a provider
  integration), reproduction steps, and impact assessment if you have one.

You will get an acknowledgement within a few business days. Please allow us a
reasonable window to ship a fix before public disclosure.

## Verifying what you download

Release binaries and container images carry build provenance signed through
GitHub's Sigstore instance, and the images are also signed with `cosign`; the
commands are in [the release process](docs/contributing/release-process.md#after-the-release).
Each release binary embeds its dependency list (`cargo audit bin <binary>`
checks it against the advisory database) and ships with a CycloneDX SBOM.

## Supported versions

The project is pre-1.0: security fixes land on the latest `main` only.

## Scope notes

- Every served surface is token-protected and the shipped deploy configs fail
  closed: workers and the orchestrator refuse to start without
  `VP_CLIENT_TOKEN` / `VP_ADMIN_TOKEN` / `VP_INTERNAL_TOKEN`, and the admin
  console backend requires `ADMIN_UI_TOKEN`. The launcher generates all four.
  Running any surface open (`[auth] required = false`,
  `ADMIN_UI_AUTH_REQUIRED=false`) is supported only on loopback. None of the
  services terminate TLS; put a reverse proxy in front for anything beyond a
  laptop. See [Deployment › Hardening checklist](docs/guides/deployment.md#hardening-checklist).
- **Data handling.** When `[pipeline.audit] enabled = true`, a worker writes
  each session's raw audio (both sides) and transcripts under `captures/` on
  the host, unencrypted, and prunes sessions older than `retention_days`
  (default 7) at startup and every six hours. One session writes at most
  `max_session_bytes` (default 512 MiB) of capture; prompts are traced as a
  count and hash unless `full_prompts = true`. The launcher's default profile
  records; the other shipped profiles do not. Session and turn metadata (no
  audio) is kept by the orchestrator's session store, bounded by its entry
  cap on disk as well as in memory. Nothing is sent anywhere except to the
  providers you configure.
- API keys are read from environment variables and are never persisted by the
  engine; the config wizard writes them only to the gitignored
  `voice-pipes/.env`, created owner-readable (mode 600 on Unix) and replaced
  atomically. Key prompts are hidden; a key never lands in a TOML file or in
  the terminal scrollback.
- **Redaction.** Configs and provider frames that leave the process (worker
  registration, the debug and orchestrator config endpoints, the parameter
  schema, the trace-level provider frame log) pass through one redactor. It
  blanks every value, of any type and at any depth, under a key that
  contains `api_key`, `apikey`, `secret`, `password`, `passwd`,
  `authorization`, `bearer`, `credential` or `private_key`, that is `token`
  or ends in `_token`, or that is exactly `key` (case-insensitive, `-` and
  `_` alike), and blanks the userinfo and secret-named query parameters of
  URL-shaped strings. It does not redact: keys ending in `_env` (they name a
  variable, not a value), `max_tokens`-style counters, secrets hidden in
  free text such as a system prompt, or files on disk (captures, `.env`).
- This repository vendors Microsoft VibeVoice (`voice-pipes/VibeVoice/`);
  vulnerabilities in VibeVoice itself should also be reported upstream to
  Microsoft.
