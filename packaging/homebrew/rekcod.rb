# Homebrew formula for a tap (TimMikeladze/homebrew-tap). Not published yet; see
# docs/releasing.md. Bump `version` and `sha256` from the release's SHA256SUMS
# (rekcod-<version>-darwin-arm64.tar.gz).
class Rekcod < Formula
  desc "Docker on Apple Silicon: upstream dockerd in a Virtualization.framework VM"
  homepage "https://github.com/TimMikeladze/rekcod"
  version "0.3.0"
  url "https://github.com/TimMikeladze/rekcod/releases/download/v#{version}/rekcod-#{version}-darwin-arm64.tar.gz"
  sha256 "REPLACE_WITH_SHA256_FROM_SHA256SUMS"
  license "Apache-2.0"

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "bin/rekcod"
    # The web dashboard, when the release bundles it (`rekcod dashboard` looks here).
    (share/"rekcod").install "share/rekcod/dashboard" if File.directory?("share/rekcod/dashboard")
  end

  def caveats
    <<~EOS
      Start the VM and switch your docker context to it:
        rekcod start
      LaunchAgents (`rekcod start --install`, `rekcod menubar --install`) point at
      #{HOMEBREW_PREFIX}/bin/rekcod, so they keep working across `brew upgrade`.
      `rekcod upgrade` refuses on a Homebrew install; use `brew upgrade rekcod`.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/rekcod --version")
  end
end
