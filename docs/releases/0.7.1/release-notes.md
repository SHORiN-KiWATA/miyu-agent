# Miyu 0.7.1

一小版：默认提示词的说话方式重写了一轮，fish 里把长文交给 AI 时不再闪一下，另外多了一个抖音/TikTok 下载脚本。

完整记录在[这儿](https://github.com/SHORiN-KiWATA/miyu-agent/blob/v0.7.1/docs/releases/0.7.1/changelog.md)。

## 变更

- **默认提示词调整**：调整了默认提示词的说话方式与自检规则。人设收成「知识丰富的助手」，发消息前先自检一次有没有冗余；被说成 AI 时不再回一句「何意味」，而是略过这个话题。示例对话也跟着换成正常回话。想用回自己那套，在 `miyu config` 里改「Miyu 人格附加」。

## 修复

- **fish 里把长文交给 AI 时不再闪一下**：粘一段长提示词回车，文字直接变红交给 AI，中间不再先被擦空再重画。重新生成一次 shell 集成后生效：`miyu fish-init`，或者等它自动更新。

## 新增

- **抖音/TikTok 下载脚本**：内置技能多了 `douyin_dl`。粘整段分享文本（带短链那种也行）就能解析，`info` 只看信息，`download` 去水印存到 `~/Downloads/douyin`，图文帖把每张图存进同一个目录。

## 升级注意

- 提示词和 fish 集成都编在二进制里，**这次要换新的 miyu 二进制**，daemon 也要重启一次才会用上新提示词。
- macOS 包没有签名，浏览器直接下载会被 Gatekeeper 拦，请用 Homebrew 安装。

## 下载与安装

附件是七个安装包：Linux x86_64 六个、macOS（Apple Silicon）一个。Linux 主包自带字体、语义模型、表情、脚本、默认知识库和许可证；语音是可选包，版本要和主包一致，已经装了语音的请在同一条命令里写上两个包。

| 系统 | 主包 | 可选语音包 |
| --- | --- | --- |
| Arch Linux | `miyu-0.7.1-1-x86_64.pkg.tar.zst` | `miyu-voice-0.7.1-1-x86_64.pkg.tar.zst` |
| Debian 13 / Ubuntu 24.04 LTS 及更新 / Linux Mint 22 | `miyu_0.7.1-1_amd64.deb` | `miyu-voice_0.7.1-1_amd64.deb` |
| Fedora 44 | `miyu-0.7.1-1.fc44.x86_64.rpm` | `miyu-voice-0.7.1-1.fc44.x86_64.rpm` |
| macOS 15+（Apple Silicon） | `miyu-0.7.1-1-aarch64-apple-darwin.tar.gz`（请用 Homebrew 装） | — |

```bash
# Arch Linux
sudo pacman -U ./miyu-0.7.1-1-x86_64.pkg.tar.zst

# Debian / Ubuntu / Linux Mint
sudo apt install ./miyu_0.7.1-1_amd64.deb

# Fedora 44
sudo dnf install ./miyu-0.7.1-1.fc44.x86_64.rpm

# macOS（Apple Silicon）
brew install shorin-kiwata/miyu/miyu
```

六个 Linux 包都在 Arch、Debian 13、Ubuntu 24.04、Ubuntu 26.04、Linux Mint 22.3、Fedora 44 的干净容器里装过一遍，macOS 包在 macOS 15 上解压验收并真装了一遍 Homebrew formula，每个都让她真回了一句话。Arch 也可以走 AUR：`miyu` / `miyu-voice` 是二进制包装，`miyu-git` 从源码构建。

<details>
<summary>SHA256 校验值（七个安装包）</summary>

```text
dd44ac9e50d86cb2df86b7f6d18610f894b0620a81f5568c55ba944bb7e28201  miyu-0.7.1-1-aarch64-apple-darwin.tar.gz
ab771fb31ba290f67a434f63063f487aa229a95688c506b357e1283a812f86f2  miyu-0.7.1-1-x86_64.pkg.tar.zst
cc4b6e50f853332479455c6aebaae1902abbd6809ec91fb0220863e66af1d01a  miyu-0.7.1-1.fc44.x86_64.rpm
e419d027a3712c1e974f7b1838c1500f51dae5fdcc1e048c4e4ca90bdd9a78c4  miyu-voice-0.7.1-1-x86_64.pkg.tar.zst
ab7840c968463ea646618bf0f2e043300e555d01b3213bf20c8f7e06934a0e19  miyu-voice-0.7.1-1.fc44.x86_64.rpm
a29f647154ae690b82820552ed88a8440d09f2c5dc36892de78da8e3b73629c6  miyu-voice_0.7.1-1_amd64.deb
df74b1f113e644bea6a9df3efb87e7bcb37b60a02ae1817d454055a0f8dbb816  miyu_0.7.1-1_amd64.deb
```

</details>
