# 真相源在 miyu-agent 的 packaging/homebrew/，tap 仓库 SHORiN-KiWATA/homebrew-miyu 是它的镜像。
# url / version / sha256 / revision 由 packaging/ci/channel_update.py 按验收过的发布包写入，
# 不要手改；全零的 sha256 表示还没有发过带 macOS 包的版本。
class Miyu < Formula
  desc "Anime girl living in your terminal: open-source AI assistant"
  homepage "https://github.com/SHORiN-KiWATA/miyu-agent"
  url "https://github.com/SHORiN-KiWATA/miyu-agent/releases/download/v0.6.2/miyu-0.6.2-1-aarch64-apple-darwin.tar.gz"
  version "0.6.2"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license all_of: ["MIT", "OFL-1.1"]

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on macos: :sequoia
  depends_on "chafa"
  depends_on "onnxruntime"
  depends_on "ripgrep"

  def install
    # 发布包本身就是安装前缀的布局；Miyu 按程序所在前缀找 share/miyu 下的资源。
    prefix.install "bin", "share"
  end

  test do
    assert_equal "miyu #{version}", shell_output("#{bin}/miyu --version").strip
    assert_match "#{share}/miyu/personas", shell_output("#{bin}/miyu paths")
  end
end
