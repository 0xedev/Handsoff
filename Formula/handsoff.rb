class Handsoff < Formula
  desc "Local control plane for AI coding agents"
  homepage "https://github.com/0xedev/Handsoff"
  url "https://github.com/0xedev/Handsoff/archive/refs/tags/v0.4.1-alpha.tar.gz"
  sha256 "2ded84000e7da61fa2534a0943731278e9d6ae0daa1dd9eef264452ec22e3712"
  license "MIT"

  depends_on "rust" => :build

  def install
    cd "rust" do
      system "cargo", "build", "--locked", "--release", "--bin", "handoff", "--manifest-path", "crates/cli/Cargo.toml"
      bin.install "target/release/handoff"
    end
  end

  test do
    assert_match "multi-agent orchestration CLI", shell_output("#{bin}/handoff --help")
  end
end
