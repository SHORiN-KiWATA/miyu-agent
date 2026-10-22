# Miyu 0.7.2

一小版：默认提示词的说话方式又改了一轮，更像在聊天软件里打字，还带一点点傲娇；腾讯 QQ 的 Token 留空时，局域网里的 NapCat 也能连上了。

完整记录在[这儿](https://github.com/SHORiN-KiWATA/miyu-agent/blob/v0.7.2/docs/releases/0.7.2/changelog.md)。

## 变更

- **默认提示词调整**：说话改成聊天软件里打字的样子——短，一条只说一件事，句末不加句号，不用 Markdown，拿不准就说「应该」「好像」。性格上加了一点真人女生那种傲娇：嘴上嫌一句照样帮，被夸不接，关心不直说，十句里露一两次。写代码、写文档时交付物照常按需要的格式和长度来。示例对话跟着补了一批日常短回话。想用回自己那套，在 `miyu config` 里改「Miyu 人格附加」。

## 修复

- **腾讯 QQ：Token 留空时局域网也能连**：以前空 Token 只接受本机连接，NapCat 放在另一台机器、容器或虚拟机里会被拒。现在本机和局域网私有地址都放行，公网来源仍然必须带 Token。
- **`miyu status` 列出实际的 QQ 连接地址**：不再写死 `ws://localhost`，而是列出本机每个 IP 的 `ws://<ip>:<端口>/ws`。

## 升级注意

- 提示词编在二进制里，**这次要换新的 miyu 二进制**，daemon 也要重启一次才会用上。
- macOS 包没有签名，浏览器直接下载会被 Gatekeeper 拦，请用 Homebrew 安装。

## 下载与安装

附件是七个安装包：Linux x86_64 六个、macOS（Apple Silicon）一个。Linux 主包自带字体、语义模型、表情、脚本、默认知识库和许可证；语音是可选包，版本要和主包一致，已经装了语音的请在同一条命令里写上两个包。

| 系统 | 主包 | 可选语音包 |
| --- | --- | --- |
| Arch Linux | `miyu-0.7.2-1-x86_64.pkg.tar.zst` | `miyu-voice-0.7.2-1-x86_64.pkg.tar.zst` |
| Debian 13 / Ubuntu 24.04 LTS 及更新 / Linux Mint 22 | `miyu_0.7.2-1_amd64.deb` | `miyu-voice_0.7.2-1_amd64.deb` |
| Fedora 44 | `miyu-0.7.2-1.fc44.x86_64.rpm` | `miyu-voice-0.7.2-1.fc44.x86_64.rpm` |
| macOS 15+（Apple Silicon） | `miyu-0.7.2-1-aarch64-apple-darwin.tar.gz`（请用 Homebrew 装） | — |

```bash
# Arch Linux
sudo pacman -U ./miyu-0.7.2-1-x86_64.pkg.tar.zst

# Debian / Ubuntu / Linux Mint
sudo apt install ./miyu_0.7.2-1_amd64.deb

# Fedora 44
sudo dnf install ./miyu-0.7.2-1.fc44.x86_64.rpm

# macOS（Apple Silicon）
brew install shorin-kiwata/miyu/miyu
```

六个 Linux 包都在 Arch、Debian 13、Ubuntu 24.04、Ubuntu 26.04、Linux Mint 22.3、Fedora 44 的干净容器里装过一遍，macOS 包在 macOS 15 上解压验收并真装了一遍 Homebrew formula，每个都让她真回了一句话。Arch 也可以走 AUR：`miyu` / `miyu-voice` 是二进制包装，`miyu-git` 从源码构建。

<details>
<summary>SHA256 校验值（七个安装包）</summary>

```text
9ca717cffc7b0d1aa2655953b404609ea59f789b60f3bcf034f11d581cd8d0fe  miyu-0.7.2-1-aarch64-apple-darwin.tar.gz
c328d9796521fedc46b3df19efe5a06b2727f16d0d67d79dcd9d09c0905f62bd  miyu-0.7.2-1-x86_64.pkg.tar.zst
4bdb80b71b9bbfe0401f9a4954d24ee157dafd535b58572655f00dcf34fd900b  miyu-0.7.2-1.fc44.x86_64.rpm
f0337bc13d8b02c5169aee01e5e16e41699616dfb8f61a6f081c5cff046ad5aa  miyu-voice-0.7.2-1-x86_64.pkg.tar.zst
a0d63a02b49019cbc385a938c237df9f3e7893b0c4b5e48b7d942f8dd4e8db16  miyu-voice-0.7.2-1.fc44.x86_64.rpm
ca4f4950992dbcd011bd6d58abb1bb73c5d86037f297950a923dfda16241e86c  miyu-voice_0.7.2-1_amd64.deb
e6d3ee47c5e966a80eecaa6ad51a4f3577be2bb32840a34559494f54be9fc0aa  miyu_0.7.2-1_amd64.deb
```

</details>
