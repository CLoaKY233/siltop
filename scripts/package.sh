#!/usr/bin/env bash
# Build and package a siltop release exactly as .github/workflows/release.yml
# does: release.yml calls this script, so a local run is a faithful dry run.
#
#   scripts/package.sh [TAG] [--no-build] [--out DIR]
#       Build (cargo build --release --locked -p siltop), generate
#       THIRD_PARTY_NOTICES.html (cargo-about), stage the binary with
#       LICENSE-MIT, LICENSE-APACHE, README.md and CHANGELOG.md, and write to DIR (default dist/):
#         siltop-TAG-macos-arm64.tar.gz   (+ .sha256)   Homebrew, curl | tar
#         siltop-TAG-macos-arm64.zip      (+ .sha256)   what gets notarized
#       TAG defaults to v<workspace version>. When TAG is vX.Y.Z[-pre] the
#       staged binary's `--version` must report X.Y.Z[-pre] (tag/Cargo drift
#       fails the release instead of shipping a mislabelled binary).
#
#   scripts/package.sh formula TAG SHA256 [REPO]
#       Print packaging/homebrew/siltop.rb with its placeholders filled.
#       REPO is the GitHub repository holding the release (default CLoaKY233/siltop).
#
# Optional, off unless set (release.yml sets them only when secrets exist):
#   SILTOP_CODESIGN_IDENTITY  "Developer ID Application: NAME (TEAMID)":
#                                codesign the binary (hardened runtime, secure
#                                timestamp) before it is archived.
#   SILTOP_NOTARY_PROFILE     a `xcrun notarytool store-credentials` profile:
#                                submit the zip and wait for "Accepted".
#   SILTOP_KEYCHAIN           keychain holding the identity and the profile
#                                (CI uses a throwaway one; default: login).
#   SILTOP_BUNDLE_ID          code signing identifier
#                                (default io.github.cloaky233.siltop).
#
# Nothing here uploads or pushes.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
formula_template="${repo_root}/packaging/homebrew/siltop.rb"
default_public_repo="CLoaKY233/siltop"

die() {
    echo "package.sh: $*" >&2
    exit 1
}

# The version in [workspace.package]: the first `version = "..."` line.
workspace_version() {
    sed -n 's/^version = "\([^"]*\)"$/\1/p' "${repo_root}/Cargo.toml" | head -n 1
}

# "v1.2.3-rc.1" -> "1.2.3-rc.1"; empty when the tag is not a version tag.
tag_version() {
    printf '%s\n' "$1" | sed -n 's/^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\([-+][0-9A-Za-z.+-]*\)\{0,1\}\)$/\1/p'
}

sha256_file() {
    # Written from inside the directory so the file names are bare and
    # `shasum -a 256 -c NAME.sha256` works wherever the pair is downloaded.
    (cd "$(dirname "$1")" && shasum -a 256 "$(basename "$1")" >"$(basename "$1").sha256")
}

render_formula() {
    local tag=$1 sha=$2 repo=${3:-$default_public_repo} version
    version=$(tag_version "$tag")
    [ -n "$version" ] || die "formula: '$tag' is not a vX.Y.Z tag"
    printf '%s\n' "$sha" | grep -Eq '^[0-9a-f]{64}$' || die "formula: '$sha' is not a SHA-256"
    printf '%s\n' "$repo" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' || die "formula: '$repo' is not OWNER/REPO"
    [ -f "$formula_template" ] || die "missing $formula_template"
    # The template's own header is for maintainers; the tap gets a short one.
    printf '# Generated for siltop %s by release.yml; edits here are overwritten.\n' "$tag"
    sed -n -e '/^class /,$p' "$formula_template" |
        sed -e "s|@TAG@|${tag}|g" \
            -e "s|@VERSION@|${version}|g" \
            -e "s|@SHA256@|${sha}|g" \
            -e "s|@REPO@|${repo}|g"
}

codesign_binary() {
    local bin=$1 id=${SILTOP_BUNDLE_ID:-io.github.cloaky233.siltop}
    local keychain=()
    [ -n "${SILTOP_KEYCHAIN:-}" ] && keychain=(--keychain "$SILTOP_KEYCHAIN")
    echo "codesign: ${SILTOP_CODESIGN_IDENTITY}" >&2
    codesign --force --options runtime --timestamp \
        --identifier "$id" ${keychain[@]+"${keychain[@]}"} \
        --sign "$SILTOP_CODESIGN_IDENTITY" "$bin"
    codesign --verify --strict --verbose=2 "$bin"
}

notarize_zip() {
    local zip=$1 out id
    local keychain=()
    [ -n "${SILTOP_KEYCHAIN:-}" ] && keychain=(--keychain "$SILTOP_KEYCHAIN")
    echo "notarize: submitting $(basename "$zip") (this waits for Apple)" >&2
    out=$(xcrun notarytool submit "$zip" --keychain-profile "$SILTOP_NOTARY_PROFILE" \
        ${keychain[@]+"${keychain[@]}"} --wait --output-format json)
    echo "$out" >&2
    id=$(printf '%s' "$out" | sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p')
    if ! printf '%s' "$out" | grep -Eq '"status" *: *"Accepted"'; then
        [ -n "$id" ] && xcrun notarytool log "$id" --keychain-profile "$SILTOP_NOTARY_PROFILE" ${keychain[@]+"${keychain[@]}"} >&2 || true
        die "notarization was not accepted"
    fi
}

package() {
    local tag="" build=1 out="${repo_root}/dist"
    while [ $# -gt 0 ]; do
        case "$1" in
            --no-build) build=0 ;;
            --out) [ $# -ge 2 ] || die "--out needs a directory"; out=$2; shift ;;
            -h|--help) sed -n '2,31p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
            -*) die "unknown option $1" ;;
            *) [ -z "$tag" ] || die "one TAG only"; tag=$1 ;;
        esac
        shift
    done
    tag=${tag:-v$(workspace_version)}
    case "$tag" in */*|.*|"") die "'$tag' is not a usable tag" ;; esac

    local name="siltop-${tag}-macos-arm64"
    local stage="${out}/${name}" bin="${repo_root}/target/release/siltop"
    command -v cargo-about >/dev/null 2>&1 || cargo about --version >/dev/null 2>&1 \
        || die "cargo-about is missing: cargo install cargo-about --locked"

    if [ "$build" -eq 1 ]; then
        (cd "$repo_root" && cargo build --release --locked -p siltop)
    fi
    [ -x "$bin" ] || die "no binary at $bin (drop --no-build)"

    mkdir -p "$out"
    rm -rf "$stage" "${out}/${name}".tar.gz* "${out}/${name}".zip*
    mkdir -p "$stage"
    (cd "$repo_root" && cargo about generate --locked --fail about.hbs -o "${stage}/THIRD_PARTY_NOTICES.html")
    cp "$bin" "${repo_root}/LICENSE-MIT" "${repo_root}/LICENSE-APACHE" "${repo_root}/README.md" \
        "${repo_root}/CHANGELOG.md" "$stage/"

    [ -n "${SILTOP_CODESIGN_IDENTITY:-}" ] && codesign_binary "${stage}/siltop"

    # The binary only runs on its own platform; elsewhere this check is skipped.
    local want reported
    want=$(tag_version "$tag")
    if [ -n "$want" ] && [ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ]; then
        reported=$("${stage}/siltop" --version)
        case "$reported" in
            "siltop ${want}"|"siltop ${want} "*) ;;
            *) die "tag $tag but the binary reports '$reported' (bump [workspace.package] version)" ;;
        esac
    fi

    # COPYFILE_DISABLE keeps macOS from adding ._ AppleDouble files.
    COPYFILE_DISABLE=1 tar -C "$out" -czf "${out}/${name}.tar.gz" "$name"
    if command -v ditto >/dev/null 2>&1; then
        ditto -c -k --norsrc --noextattr --noqtn --noacl --keepParent "$stage" "${out}/${name}.zip"
    else
        (cd "$out" && zip -qry "${name}.zip" "$name")
    fi

    [ -n "${SILTOP_NOTARY_PROFILE:-}" ] && notarize_zip "${out}/${name}.zip"

    sha256_file "${out}/${name}.tar.gz"
    sha256_file "${out}/${name}.zip"
    (cd "$out" && shasum -a 256 -c "${name}.tar.gz.sha256" "${name}.zip.sha256" >&2)
    rm -rf "$stage"

    ls -l "${out}/${name}".* >&2
}

case "${1:-}" in
    formula) shift; [ $# -ge 2 ] && [ $# -le 3 ] || die "usage: package.sh formula TAG SHA256 [REPO]"; render_formula "$@" ;;
    *) package "$@" ;;
esac
