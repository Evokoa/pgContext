# typed: strict
# frozen_string_literal: true

# Formula for the cargo-pgrx build tool used by pgContext.
class PgrxAT0191 < Formula
  desc "Build Postgres extensions with Rust"
  homepage "https://github.com/pgcentralfoundation/pgrx"
  url "https://github.com/pgcentralfoundation/pgrx/archive/refs/tags/v0.19.1.tar.gz"
  sha256 "db105c96543559056ae8026ffa7754445883402aeb85fb62325b7072be4e911a"
  license "MIT"

  keg_only :versioned_formula

  depends_on "pkgconf" => :build
  depends_on "rust" => :build
  depends_on "openssl@3"

  on_linux do
    depends_on "zlib-ng-compat"
  end

  def install
    system "cargo", "install", *std_cargo_args(path: "cargo-pgrx")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/cargo-pgrx --version")
  end
end
