# Releasing Cartograph

Semantic version application and tags are automation-owned; see ADR-0015 and
the release rules in `AGENTS.md`. macOS bundle trust is governed by ADR-0016.

## Cut and publish a release

1. Merge behavior changes with their required Changesets. The **Version cut**
   workflow keeps the `changeset-release/main` **Version packages** PR current.
2. Review that PR, require CI to pass, and merge it. This merge is the release
   decision: automation creates the immutable annotated `vX.Y.Z` tag and hands
   it to the **Release** workflow.
3. The Release workflow validates that exact tag and reviewed PR, runs the
   reusable macOS package workflow, and publishes only its named artifact. The
   release notes come from the exact version section generated in
   `CHANGELOG.md`.

With the complete five-secret set below, the result is a normal GitHub Release
containing signed, notarized, stapled, Gatekeeper-verified universal app and DMG
assets. With no signing secrets, it is an explicitly titled and warned
`unsigned-dev` prerelease. A partial secret set fails before packaging.

Publication is idempotent. Dispatching **Release** again with the same existing
tag updates the release's title, notes, flags, and assets in place. It replaces
same-named assets and removes stale assets from the opposite signing mode; it
does not create another release or move the tag.

## Recovery and verification

If the reviewed version merge created its tag but the release handoff failed,
run **Version cut** manually. It requires the existing tag and redispatches the
missing release. You may instead dispatch **Release** with that exact tag. Do
not create, delete, or move a tag by hand.

After publication, verify the workflow and release:

```sh
gh run list --workflow release.yml --limit 5
gh release view vX.Y.Z --json tagName,isDraft,isPrerelease,name,assets
gh release download vX.Y.Z --pattern 'Cartograph_*_universal_*' --dir dist
```

For a production release, require `isDraft: false`, `isPrerelease: false`, and
exactly the versioned `signed` app zip and DMG. Then perform the manual
Gatekeeper installation smoke test below and record the evidence on the release
task.

## macOS packaging

Run the **Package macOS** workflow manually for a branch, tag, or commit SHA.
The optional `ref` input is checked out exactly; when omitted, the triggering
ref is used. The workflow installs locked root/UI dependencies, runs every
repository gate, and builds universal Intel + Apple Silicon `.app` and `.dmg`
bundles.

With no signing secrets, the workflow uses an ad-hoc signature required by
Apple Silicon and uploads artifacts whose names contain `unsigned-dev`. These
are development artifacts, do not pass Gatekeeper as trusted downloads, and
must never be promoted as production releases.

## Required GitHub secrets

Add all five repository Actions secrets together. A partial set fails closed:

| Secret | Value |
|---|---|
| `CSC_LINK` | Base64-encoded Developer ID Application `.p12` export |
| `CSC_KEY_PASSWORD` | Password chosen when exporting the `.p12` |
| `APPLE_API_KEY` | Base64-encoded App Store Connect API `.p8` private key |
| `APPLE_API_KEY_ID` | App Store Connect API Key ID |
| `APPLE_API_ISSUER` | App Store Connect Issuer ID |

The names match `qwts/photos`. The workflow maps them to Tauri's documented
Apple inputs and decodes the API key into a permission-restricted temporary
file. It never prints secret material. Tauri manages the temporary certificate
keychain, so no sixth keychain-password secret is required.

The signed path must pass all of these before upload:

- strict deep signature validation of `Cartograph.app` and signature validation
  of the DMG;
- Gatekeeper assessment of the app and disk image;
- notarization ticket/staple validation of the app and disk image.

Tauri submits and staples the application during its build. After the signed
DMG is assembled, the workflow submits that outer disk image to Apple's notary
service, waits for acceptance, and staples its ticket before running the final
checks above.

## Manual Gatekeeper installation smoke test

Perform this procedure on the versioned `signed` DMG downloaded from GitHub
Actions. Record the workflow URL, artifact filename, macOS version, hardware
architecture, and result in issue #92 (or the release task that supersedes it).

1. Confirm the filename contains the expected version, `universal`, and
   `signed`, and does not contain `unsigned-dev`.
2. Run `spctl --assess --type open --context context:primary-signature -vv
   Cartograph_*.dmg`; require an accepted/notarized result.
3. Open the DMG, drag Cartograph to Applications, eject the DMG, and launch
   Cartograph from Applications without using Control-click or a security
   override.
4. Confirm no unidentified-developer, damaged-app, or notarization warning is
   shown and the Cartograph window opens.
5. Run `codesign --verify --deep --strict -vv /Applications/Cartograph.app` and
   `xcrun stapler validate /Applications/Cartograph.app`; require both to pass.
6. On an Apple Silicon Mac, run `lipo -archs
   /Applications/Cartograph.app/Contents/MacOS/app` and require both `x86_64`
   and `arm64`.

The credential-free workflow exercise can validate configuration and artifact
shape, but it cannot satisfy this production smoke test.
