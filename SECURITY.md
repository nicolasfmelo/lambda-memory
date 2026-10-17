# Security policy

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x | yes |

## Reporting a vulnerability

Report vulnerabilities privately through [GitHub Security Advisories](https://github.com/nicolasfmelo/lambda-memory/security/advisories/new). Do not open a public issue.

Include the version or commit, your setup (mode, client), steps to reproduce and the impact you expect. Please allow reasonable time for a fix before disclosing details.

## Scope and threat model

λambda memory is designed for a single user on one Linux machine.

- The service listens on a Unix socket with mode 600 inside a private directory. Configuration and provider files must be mode 600; the service refuses files other users can read.
- MCP sessions authenticate with a shared token from the private configuration. The bridge adds it for clients; it is never sent to the agent or written to logs.
- The optional web UI listens on `127.0.0.1` only, checks that the connecting process runs as the same user, requires same-origin `Host`/`Origin` headers and a per-start CSRF token, sends no CORS headers and uses a strict Content-Security-Policy. Any process running as your user can still reach it.
- In `openrouter` mode, captured text and recall questions are sent to OpenRouter (see the README's Privacy section).

In scope: bypassing project scoping or authentication, reaching the socket or web UI from another user or from a web page, leaking the token or API key, purge leaving recallable data, and memory content escaping its project.

Out of scope: attacks that already run code as the same user, prompt injection that only affects how an agent uses recalled text (recalled text is documented as untrusted), and provider-side data handling.
