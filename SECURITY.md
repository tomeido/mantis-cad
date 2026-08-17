# Security Policy

## Supported versions

Security fixes are provided for the latest tagged release and the current
`main` branch. Preview archives are not code-signed; verify every download
against `SHA256SUMS` in its GitHub Release.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting for this repository. Do
not open a public issue for an undisclosed vulnerability. Include the affected
version or image digest, reproduction steps, impact, and any suggested
mitigation. You should receive an acknowledgement within seven days.

## Deployment notes

- An Ed25519 signature proves who authored a block; write authorization is
  enforced separately by each project's owner/writer allowlist.
- Keep identity and operator secret keys off the server. Only public keys
  belong in `MANTIS_OPERATOR_KEYS`. Retain historical operator public keys that
  signed existing project creation proofs; key rotation is additive.
- Browser identities remain in that browser's IndexedDB. The live IndexedDB
  catalog is plaintext at rest and relies on the browser/OS profile boundary;
  the passphrase-encrypted `.mantis-key` backup does not retroactively encrypt
  it. Use a dedicated protected profile on shared machines, back up the key,
  and keep its passphrase separately. Workspace exports intentionally contain
  no signing key.
- The container runs as an unprivileged user. The supplied Compose profile
  makes the root filesystem read-only and leaves only `/data` and `/tmp`
  writable.
- Terminate TLS at the hosting platform and keep `MANTIS_ALLOWED_ORIGINS`
  empty for same-origin deployments.
- Keep the service behind an edge that enforces connection timeouts, request
  and bandwidth limits, and abuse controls. The built-in signature-aware write
  limiter protects project mutations; it is not a general DDoS firewall.
- Treat `persistence_uncertain` as an operational stop: the candidate may
  already be visible. Do not retry writes until the service has been restarted
  and the stored heads and full audit have been checked.
