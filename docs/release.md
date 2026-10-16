# Releasing siltop

How a tag in this repository becomes a GitHub release and a `brew install`.
Everything outward-facing (creating the tap, adding secrets, buying an Apple
certificate) is a one-time manual step for the maintainer; the release
workflow does the rest and quietly skips any part whose secrets are missing.

## The shape

| Repository | Holds |
| --- | --- |
| `CLoaKY233/siltop` | source, CI, the release workflow, and one GitHub release per tag with the tarball, the zip and their `.sha256` files |
| `CLoaKY233/homebrew-tap` | `Formula/siltop.rb`, a bottled formula pointing at the release tarball |

Users run `brew install cloaky233/tap/siltop` (Homebrew maps `cloaky233/tap` to
`github.com/CLoaKY233/homebrew-tap`), download the zip from the releases page,
or build from source with `cargo install --git`.

```
tag vX.Y.Z
  └─ release.yml
       gate      the full CI gate (ci.yml)
       build     cargo build → scripts/package.sh → codesign? → notarize?
       publish   GitHub release in this repository, notes from CHANGELOG.md
       homebrew  homebrew.yml, final releases only (HOMEBREW_TAP_DEPLOY_KEY):
                 bottle the release tarball on macOS, check it pours and passes
                 `brew test`, attach siltop-X.Y.Z.all.bottle.tar.gz to the
                 release, push Formula/siltop.rb with the bottle block,
                 then a clean `brew install` from the tap must pour it
```

## One-time setup

### 1. Homebrew tap

1. `CLoaKY233/homebrew-tap` is a public repository (the `homebrew-` prefix is
   required) with a `Formula/` directory.
2. Make an SSH key pair (`ssh-keygen -t ed25519 -N "" -f tap_key`), add the
   public half to the tap as a deploy key with write access, and store the
   private half in this repository as the Actions secret `HOMEBREW_TAP_DEPLOY_KEY`.
   The key can write to the tap and nothing else. Without it, releases still
   publish; only the tap bump is skipped.
3. Optional: a `HOMEBREW_TAP_REPO` variable for a different tap repository.

The formula is rendered from [`packaging/homebrew/siltop.rb`](../packaging/homebrew/siltop.rb)
by `scripts/package.sh formula <TAG> <SHA256> [OWNER/REPO]`, then
[`homebrew.yml`](../.github/workflows/homebrew.yml) adds the `bottle do` block.

**Why a bottle.** Without one, Homebrew treats the formula as a source build and
refuses it when the Command Line Tools are older than the OS, even though the
formula only copies a prebuilt binary. The bottle is tagged `all` (one arm64
binary for every supported macOS). Re-bottle an existing release by hand:
`gh workflow run homebrew.yml -f tag=vX.Y.Z` (dry run: builds, checks the pour,
uploads the bottle and formula as a run artifact, publishes nothing), then
`-f publish=true`. The tap is only bumped for final releases (not `alpha`,
`beta`, `rc` or `v0.0.*`), and only after the workflow has downloaded the
release tarball back and checked its SHA-256, so the formula never points at
bytes it has not seen.

### 2. Apple Developer ID (codesign + notarization)

Needs a paid Apple Developer Program membership (99 USD a year).

1. **Certificate.** In Xcode → Settings → Accounts → Manage Certificates, or at
   developer.apple.com → Certificates, create a **Developer ID Application**
   certificate. (A Developer ID *Installer* certificate is only needed for a
   `.pkg`; see below.)
2. **Export it** from Keychain Access (certificate *and* private key) as a
   `.p12` with a strong password. Then add three Actions secrets:
   - `APPLE_DEVELOPER_ID_P12`: `base64 -i DeveloperID.p12 | pbcopy`
   - `APPLE_DEVELOPER_ID_P12_PASSWORD`: the export password
   - `APPLE_DEVELOPER_ID`: the identity exactly as
     `security find-identity -v -p codesigning` prints it, e.g.
     `Developer ID Application: Lay Sheth (TEAMID1234)`
3. **Notary credentials.** App Store Connect → Users and Access → Integrations
   → App Store Connect API → generate a key with the *Developer* role. Add:
   - `APPLE_NOTARY_KEY`: `base64 -i AuthKey_XXXXXX.p8 | pbcopy`
   - `APPLE_NOTARY_KEY_ID`: the key ID
   - `APPLE_NOTARY_ISSUER`: the issuer UUID

   (An Apple ID with an app-specific password works too for local use:
   `xcrun notarytool store-credentials siltop-notary --apple-id … --team-id … --password …`.)

With the certificate secrets present the build job imports them into a
throwaway keychain, and `scripts/package.sh` signs the binary before it is
archived:

```sh
codesign --force --options runtime --timestamp \
  --identifier io.github.cloaky233.siltop \
  --sign "Developer ID Application: … (TEAMID)" siltop
codesign --verify --strict --verbose=2 siltop
```

`--options runtime` (hardened runtime) and `--timestamp` (Apple's secure
timestamp) are both required for notarization. siltop needs no
entitlements: it reads IOKit, sysctl and `proc_pid*` data that is open to an
unprivileged, non-sandboxed process.

With the notary secrets present too, the workflow stores a notarytool profile
in the same keychain and the script submits the **zip** and waits:

```sh
xcrun notarytool submit siltop-vX.Y.Z-macos-arm64.zip \
  --keychain-profile siltop-notary --wait
```

Anything but `Accepted` fails the release and prints `notarytool log`.

**Stapling is not possible for a bare Mach-O binary** (`stapler` only staples
app bundles, disk images and flat installer packages), and a zip or tarball
cannot carry a ticket either. That is fine: notarization registers the
binary's code-directory hash with Apple, and Gatekeeper looks the ticket up
online the first time the binary runs. The tarball and the zip contain the
same signed binary, so both are covered by the one submission.

## Gatekeeper, from the user's side

| How it was installed | Quarantined? | What happens |
| --- | --- | --- |
| `brew install cloaky233/tap/siltop` | no (formula downloads are not quarantined) | runs |
| zip or tarball from a browser, **notarized** | yes | runs; Gatekeeper checks the ticket online on first launch |
| zip or tarball from a browser, **not notarized** | yes | blocked: "cannot be opened because Apple cannot check it for malicious software" |
| `curl -LO` then `tar -xzf` | no | runs |

The fallback for an unnotarized build (or an offline first launch), documented
in the public README:

```sh
xattr -d com.apple.quarantine ./siltop     # or: xattr -dr com.apple.quarantine siltop-vX.Y.Z-macos-arm64
```

On macOS 15 and later, System Settings → Privacy & Security → "Open Anyway"
also works after the first blocked attempt; the Control-click → Open shortcut
no longer does.

**Recommendation.** Ship a notarized zip (browser downloads) plus the tarball
(Homebrew). A signed, notarized and **stapled** `.pkg` is the one format that
also works offline on first launch; it is worth adding if users report
Gatekeeper trouble:

```sh
pkgbuild --root pkgroot --identifier io.github.cloaky233.siltop \
  --version X.Y.Z --install-location /usr/local/bin siltop-unsigned.pkg
productsign --sign "Developer ID Installer: … (TEAMID)" siltop-unsigned.pkg siltop-vX.Y.Z.pkg
xcrun notarytool submit siltop-vX.Y.Z.pkg --keychain-profile siltop-notary --wait
xcrun stapler staple siltop-vX.Y.Z.pkg
spctl --assess --type install -vv siltop-vX.Y.Z.pkg
```

(`pkgroot/` holds just the signed `siltop`. This needs the Developer ID
Installer certificate and is not wired into the workflow yet.)

## Cutting a release

1. Bump `[workspace.package] version` in `Cargo.toml`, move the `Unreleased`
   section of `CHANGELOG.md` under `## [X.Y.Z] - YYYY-MM-DD` (its body becomes
   the release notes), commit, and merge to `master`. If anything
   visible changed since the last release, re-capture the three README images
   in `docs/media/` first (recipe at the end of the README's Development
   section) and check they show the current footer, corner marks and panels.
2. Dry run on a Mac: `scripts/package.sh` (builds, writes `dist/`, checks the
   binary reports the tag's version, and verifies both checksums). Signing and
   notarizing run locally too if you export `SILTOP_CODESIGN_IDENTITY` and
   `SILTOP_NOTARY_PROFILE`.
3. `git tag vX.Y.Z && git push origin vX.Y.Z`. The workflow gates, builds,
   packages and publishes. A tag that does not match the Cargo version fails at
   packaging, before anything is published.
4. Check: the release has four assets; `brew update && brew upgrade
   siltop` picks up the new version (final releases only); on a clean Mac, a
   browser-downloaded zip runs without a Gatekeeper prompt (notarized builds).

To check a rendered formula before the first tap push:

```sh
scripts/package.sh formula vX.Y.Z "$(cut -d' ' -f1 dist/siltop-vX.Y.Z-macos-arm64.tar.gz.sha256)" > /tmp/siltop.rb
ruby -c /tmp/siltop.rb && brew style /tmp/siltop.rb
```


## Secrets and variables at a glance

| Name | Kind | Enables |
| --- | --- | --- |
| `APPLE_DEVELOPER_ID_P12`, `APPLE_DEVELOPER_ID_P12_PASSWORD`, `APPLE_DEVELOPER_ID` | secret, optional | codesign |
| `APPLE_NOTARY_KEY`, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER` | secret, optional | notarization (needs codesign) |
| `HOMEBREW_TAP_DEPLOY_KEY` | secret, optional | tap bump (SSH deploy key, write access to the tap only) |
| `HOMEBREW_TAP_REPO` | variable, optional | another tap repository |
