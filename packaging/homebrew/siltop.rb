# Homebrew formula TEMPLATE for siltop: the prebuilt release binary, bottled.
#
# Do not edit the placeholders by hand. release.yml renders this file with
#   scripts/package.sh formula <TAG> <SHA256> [OWNER/REPO]
# (@TAG@, @VERSION@, @SHA256@, @REPO@ -> the GitHub repository holding the release),
# builds a bottle from it on macOS, adds the `bottle do` block and commits the result
# to the tap as Formula/siltop.rb. See docs/release.md.
#
# Users install with `brew install cloaky233/tap/siltop`.
class Siltop < Formula
  desc "Apple Silicon system monitor for the terminal: watts, clocks and temperatures"
  homepage "https://github.com/@REPO@"
  url "https://github.com/@REPO@/releases/download/@TAG@/siltop-@TAG@-macos-arm64.tar.gz"
  sha256 "@SHA256@"
  license any_of: ["MIT", "Apache-2.0"]

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "siltop"
    generate_completions_from_executable(bin/"siltop", "completions")
    (man1/"siltop.1").write Utils.safe_popen_read(bin/"siltop", "man")
    doc.install "README.md", "CHANGELOG.md", "THIRD_PARTY_NOTICES.html"
    prefix.install "LICENSE-MIT", "LICENSE-APACHE"
  end

  def caveats
    <<~EOS
      siltop reads everything it shows without sudo. Its only network request is an
      optional daily update check, off unless you turn it on (onboarding or settings).
      `siltop doctor` shows what this Mac exposes; `siltop --demo` plays a canned
      60 s session.
    EOS
  end

  test do
    assert_match "siltop #{version}", shell_output("#{bin}/siltop --version")
    # Hardware-free: the demo session prints one JSON sample.
    assert_match "\"schema_version\":1", shell_output("#{bin}/siltop --demo --json --procs 1")
  end
end
