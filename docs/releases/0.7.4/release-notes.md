# Miyu 0.7.4

一小版：默认提示词的说话方式再调一轮，接话更像群里随口一句，不随便科普。

完整记录在[这儿](https://github.com/SHORiN-KiWATA/miyu-agent/blob/v0.7.4/docs/releases/0.7.4/changelog.md)。

## 变更

- **默认提示词调整**：接话多半是表个态、起个哄、好奇问一句或者说说自己，别人没问「是什么」「怎么弄」就不科普、不纠正细节；短反应多了「啊？」「绷」「真的假的」，一半左右的话在七个字以内。示例对话补了几组短回话。想用回自己那套，在 `miyu config` 里改「Miyu 人格附加」。

## 升级注意

- 提示词编在二进制里，**这次要换新的 miyu 二进制**，daemon 也要重启一次才会用上。
- macOS 包没有签名，浏览器直接下载会被 Gatekeeper 拦，请用 Homebrew 安装。

## 下载与安装

附件是七个安装包：Linux x86_64 六个、macOS（Apple Silicon）一个。Linux 主包自带字体、语义模型、表情、脚本、默认知识库和许可证；语音是可选包，版本要和主包一致，已经装了语音的请在同一条命令里写上两个包。

| 系统 | 主包 | 可选语音包 |
| --- | --- | --- |
| Arch Linux | `miyu-0.7.4-1-x86_64.pkg.tar.zst` | `miyu-voice-0.7.4-1-x86_64.pkg.tar.zst` |
| Debian 13 / Ubuntu 24.04 LTS 及更新 / Linux Mint 22 | `miyu_0.7.4-1_amd64.deb` | `miyu-voice_0.7.4-1_amd64.deb` |
| Fedora 44 | `miyu-0.7.4-1.fc44.x86_64.rpm` | `miyu-voice-0.7.4-1.fc44.x86_64.rpm` |
| macOS 15+（Apple Silicon） | `miyu-0.7.4-1-aarch64-apple-darwin.tar.gz`（请用 Homebrew 装） | — |

```bash
# Arch Linux
sudo pacman -U ./miyu-0.7.4-1-x86_64.pkg.tar.zst

# Debian / Ubuntu / Linux Mint
sudo apt install ./miyu_0.7.4-1_amd64.deb

# Fedora 44
sudo dnf install ./miyu-0.7.4-1.fc44.x86_64.rpm

# macOS（Apple Silicon）
brew install shorin-kiwata/miyu/miyu
```

六个 Linux 包都在 Arch、Debian 13、Ubuntu 24.04、Ubuntu 26.04、Linux Mint 22.3、Fedora 44 的干净容器里装过一遍，macOS 包在 macOS 15 上解压验收并真装了一遍 Homebrew formula，每个都让她真回了一句话。Arch 也可以走 AUR：`miyu` / `miyu-voice` 是二进制包装，`miyu-git` 从源码构建。

<details>
<summary>SHA256 校验值（七个安装包）</summary>

```text
1cfb73800d883a06aaedfed8c93ade067803cde0fa43464bf9947b2b224c61f3  miyu-0.7.4-1-aarch64-apple-darwin.tar.gz
ad020e4a226407837f0caf130e88842133ff26ed9b5486f0cf4d0488a6a28ba8  miyu-0.7.4-1-x86_64.pkg.tar.zst
3da4ad7ba0fa267f6462fcffc4a0014502e3791eabe1029d44236867877f34ed  miyu-0.7.4-1.fc44.x86_64.rpm
8d4ff373cc5b17a71d7ef2b7cb6e38970791739dc08e090bf040a8667af40bac  miyu-voice-0.7.4-1-x86_64.pkg.tar.zst
fc19efc223e9df98578e501223fe7a695012b7b04b28ffffbb3e5fc52321b2a3  miyu-voice-0.7.4-1.fc44.x86_64.rpm
b0d40dea31c4b3b312dda90f2bdab3e0c0ee9ea503c8069f78340587ea53734b  miyu-voice_0.7.4-1_amd64.deb
c68f1ed902046bfa7335a163658f91d9a38ad59bd481148a2ce245a93ede8676  miyu_0.7.4-1_amd64.deb
```

</details>
