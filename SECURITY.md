# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in compact-lsp, please report it
privately.

**Do not open a public GitHub issue for a suspected vulnerability.**

Instead, please report security issues by:

1. Use GitHub's private vulnerability reporting for `lowhung/compact-lsp`.
2. If private reporting is unavailable, contact the maintainer through the
   contact method on the [`lowhung`](https://github.com/lowhung) profile.

### What to Include

When reporting a vulnerability, please include:

- A description of the vulnerability.
- Affected versions and platforms.
- Steps to reproduce or a minimal proof of concept.
- Potential impact.
- A suggested fix, if available.
- Whether the report concerns the server, extension installer, release
  artifacts, or Compact compiler integration.

Do not include production secrets, private contracts, or unredacted local
paths.

This community project does not promise a fixed response SLA. Reports will be
acknowledged and triaged as maintainer availability permits.

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.2.x public beta | Yes |
| Earlier versions | No |

## Security Best Practices

When using compact-lsp:

- Install the extension and server only from this repository's releases.
- Verify downloaded artifacts against `SHA256SUMS` or GitHub provenance.
- Keep the Compact CLI and selected toolchain current within the supported 0.33
  line.
- Review compiler output and generated contract artifacts before deployment.
