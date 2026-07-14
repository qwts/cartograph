# ADR-0016 — Fail-closed macOS Developer ID distribution

- **Status:** Accepted
- **Date:** 2026-07-13
- **Deciders:** Cartograph maintainers

## Context

Cartograph's primary distribution target is macOS on both Intel and Apple
Silicon. Browser-downloaded applications need a valid Developer ID signature
and Apple notarization to pass Gatekeeper without asking users to bypass a
security warning. CI must also support packaging before credentials are
provisioned without allowing an unsigned development artifact to look like a
production release.

The existing Photos repository uses five GitHub secrets for the Developer ID
certificate and App Store Connect API key. Reusing that contract avoids a
second credential convention, while Tauri expects differently named runtime
environment variables.

## Decision

- macOS distribution targets one universal binary containing
  `aarch64-apple-darwin` and `x86_64-apple-darwin`, packaged as both `.app` and
  `.dmg`. Tauri bundling uses the existing `com.qwtm.cartograph` identifier,
  generated platform icons, a Developer Tool category, a macOS 10.13 minimum,
  and hardened runtime.
- The reusable packaging workflow accepts an exact ref and runs the repository's
  required gates before bundling it.
- Production signing requires exactly five repository secrets: `CSC_LINK`,
  `CSC_KEY_PASSWORD`, `APPLE_API_KEY`, `APPLE_API_KEY_ID`, and
  `APPLE_API_ISSUER`. CI maps them to Tauri's `APPLE_CERTIFICATE`,
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_API_KEY_PATH`, `APPLE_API_KEY`, and
  `APPLE_API_ISSUER` inputs. No keychain-password secret is needed.
- The zero-secret state builds with Tauri's ad-hoc identity and is named
  `unsigned-dev`. Any partial set fails before the build. Only the complete set
  can enter the production path.
- The production path must pass strict `codesign` validation, Gatekeeper
  assessment, and stapling validation for its bundles before upload. Artifacts
  include the synchronized application version, universal architecture, and
  signing mode in stable names.

## Consequences

- One artifact runs natively on supported Intel and Apple Silicon Macs.
- Invalid or incomplete credentials cannot silently downgrade a production
  build to unsigned output.
- Credential-free packaging remains available for CI and development, but its
  names and verification boundary make its non-production status explicit.
- Signing and notarization cannot be fully exercised until maintainers add the
  five repository secrets.

## Alternatives (≤3)

- **Separate Intel and Apple Silicon artifacts** — rejected because it shifts
  architecture selection to users and doubles release assets.
- **Allow certificate-only signing** — rejected because Developer ID software
  without successful notarization still fails the intended Gatekeeper promise.
- **Require credentials for every packaging run** — rejected because it blocks
  safe workflow validation and contributor-built development artifacts.
