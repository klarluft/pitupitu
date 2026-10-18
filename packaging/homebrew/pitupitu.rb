# The Homebrew formula for pitupitu, as a template.
#
# The release workflow fills in the version and the four checksums from the
# tarballs it has just built (`fill.sh` next to this file) and attaches the
# result to the release as `pitupitu.rb`. klarluft/homebrew-tap copies that
# file in as it is, so the formula and the binaries it points at come from
# the same run. Homebrew reads the version from the `v…` in the URLs.
class Pitupitu < Formula
  desc "Links this machine's coding agents to pitupitu, a voice foreman"
  homepage "https://pitupitu.chat"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/klarluft/pitupitu/releases/download/v__VERSION__/pitupitu-aarch64-apple-darwin.tar.gz"
      sha256 "__SHA256_AARCH64_APPLE_DARWIN__"
    end
    on_intel do
      url "https://github.com/klarluft/pitupitu/releases/download/v__VERSION__/pitupitu-x86_64-apple-darwin.tar.gz"
      sha256 "__SHA256_X86_64_APPLE_DARWIN__"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/klarluft/pitupitu/releases/download/v__VERSION__/pitupitu-aarch64-unknown-linux-musl.tar.gz"
      sha256 "__SHA256_AARCH64_UNKNOWN_LINUX_MUSL__"
    end
    on_intel do
      url "https://github.com/klarluft/pitupitu/releases/download/v__VERSION__/pitupitu-x86_64-unknown-linux-musl.tar.gz"
      sha256 "__SHA256_X86_64_UNKNOWN_LINUX_MUSL__"
    end
  end

  def install
    bin.install "pitupitu"
  end

  def caveats
    <<~EOS
      To sign this machine in and start pitupitu now and at every login:

        pitupitu setup

      After `brew upgrade pitupitu`, run `pitupitu setup` again so the
      service restarts on the new version. Before `brew uninstall pitupitu`,
      run `pitupitu uninstall` to remove the service.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/pitupitu --version")
  end
end
