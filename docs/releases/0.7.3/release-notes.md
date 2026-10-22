# Miyu 0.7.3

一小版：QQ 群里几个人接连复读同一句话时，她只回一次；主动回复的几项加分减半，插话少一些。

完整记录在[这儿](https://github.com/SHORiN-KiWATA/miyu-agent/blob/v0.7.3/docs/releases/0.7.3/changelog.md)。

## 变更

- **QQ 群里主动回复的加分减半**：续聊、刚说完话的观察窗口、名字或 @ 叫到她这三项加分都减半，她插话会少一些。配置里已经写了具体数值的保持原样。

## 修复

- **群里复读只回一次**：几个人接连发同一句话（比如一起刷「miyu似了」），以前她会挨个回、每句都引用加 @；现在只有第一条走正常判断，后面跟着复读的直接略过，回第一条时也不再引用和 @ 某一个人。判断要不要回时，她也能看到自己已经决定要回、还没发出去的那几条，意思一样的话不再重复接。

## 升级注意

- 这次要换新的 miyu 二进制，daemon 也要重启一次才会生效。
- macOS 包没有签名，浏览器直接下载会被 Gatekeeper 拦，请用 Homebrew 安装。

## 下载与安装

附件是七个安装包：Linux x86_64 六个、macOS（Apple Silicon）一个。Linux 主包自带字体、语义模型、表情、脚本、默认知识库和许可证；语音是可选包，版本要和主包一致，已经装了语音的请在同一条命令里写上两个包。

| 系统 | 主包 | 可选语音包 |
| --- | --- | --- |
| Arch Linux | `miyu-0.7.3-1-x86_64.pkg.tar.zst` | `miyu-voice-0.7.3-1-x86_64.pkg.tar.zst` |
| Debian 13 / Ubuntu 24.04 LTS 及更新 / Linux Mint 22 | `miyu_0.7.3-1_amd64.deb` | `miyu-voice_0.7.3-1_amd64.deb` |
| Fedora 44 | `miyu-0.7.3-1.fc44.x86_64.rpm` | `miyu-voice-0.7.3-1.fc44.x86_64.rpm` |
| macOS 15+（Apple Silicon） | `miyu-0.7.3-1-aarch64-apple-darwin.tar.gz`（请用 Homebrew 装） | — |

```bash
# Arch Linux
sudo pacman -U ./miyu-0.7.3-1-x86_64.pkg.tar.zst

# Debian / Ubuntu / Linux Mint
sudo apt install ./miyu_0.7.3-1_amd64.deb

# Fedora 44
sudo dnf install ./miyu-0.7.3-1.fc44.x86_64.rpm

# macOS（Apple Silicon）
brew install shorin-kiwata/miyu/miyu
```

六个 Linux 包都在 Arch、Debian 13、Ubuntu 24.04、Ubuntu 26.04、Linux Mint 22.3、Fedora 44 的干净容器里装过一遍，macOS 包在 macOS 15 上解压验收并真装了一遍 Homebrew formula，每个都让她真回了一句话。Arch 也可以走 AUR：`miyu` / `miyu-voice` 是二进制包装，`miyu-git` 从源码构建。
