# Packaging

`common/assets.json` 是字体、模型、表情、脚本、知识库和许可证的共同清单。
`ci/stage.py` 用它生成完整目录，GNU DEB/RPM/tar 与 Arch 包都从这个目录打包。
资源随主包提供，voice 是可选包，不存在独立 assets 包。

Linux 0.6.1 已发布并通过容器安装和真实模型输出验收：Arch x86_64、Debian 13、
Ubuntu 24.04 LTS/26.04、Linux Mint 22.3、固定的 Fedora 稳定版。2026-09-23 起发版用
`smoke` profile，多一个 `macos-arm64`（Apple Silicon、macOS 15+，只有主程序）：包在
GitHub Actions 的 macos-15 runner 上原生构建与验收，经 Homebrew 分发（见下文）。
具体输入、资产和必需检查以冻结的 release input 与报告为准。

**2026-09-20 起 GNU 渠道的支持下限是 Ubuntu 24.04 LTS**（此前是 25.10）。下限由构建
基座决定：GNU builder 从 Debian 13（glibc 2.41）换成 `ubuntu:24.04`（glibc 2.39），
包里的 `libc6` 下限不再写字面量、从 `common/toolchain.lock.json` 的
`builders.gnu-x86_64.glibc` 读。安装验收的两个 Ubuntu 目标改成一头一尾：
`ubuntu2404-x86_64`（下限）与 `ubuntu2604-x86_64`（最新）。同一份 DEB 另外在
`mint22-x86_64` 上验收——Linux Mint 22.x 的基座正是 Ubuntu 24.04，镜像固定在最新
稳定版 22.3（zena）。它是 Ubuntu 派生但自带一套软件源，装 DEB 时拉的依赖
（`ripgrep`、`chafa`、`libasound2t64`）来自 Mint 自己的镜像，值得单独装一次证明。

GitHub Release 附件提供 Arch、DEB、RPM 三种格式的主包与可选 voice 包，外加 Homebrew
formula 下载的 macOS 主程序 tar.gz，共七个（`linux-smoke` 为前六个）。
Debian、两个 Ubuntu 与 Linux Mint 共用同一个 DEB，不需要为每个发行版重复上传。GNU tar、OOBE
截图、SHA256SUMS、验收 JSON、SBOM、provenance、release input 和 release manifest
保留在完整验证 bundle 或 CI artifact 中，不上传 Release。OOBE 截图从仓库资源嵌入
发布说明。公开附件名单由冻结资产清单中的 `archlinux`、`deb`、`rpm` 格式与 `macos-arm64`
构建的包选出（`lib/release_bundle.py::is_public`）；内部文件仍须通过完整性和验收检查，
`publish.py --dry-run` 只列实际公开的包。

## Homebrew（macOS）

| 位置 | 用途 |
|---|---|
| `homebrew/Formula/miyu.rb` | formula 真相源。url / version / sha256 / revision 只由 `ci/channel_update.py` 按验收过的包写入 |
| `homebrew/README.md` | tap 仓库的 README |
| `SHORiN-KiWATA/homebrew-miyu` | tap 仓库，是上面两个文件的镜像；用户 `brew install shorin-kiwata/miyu/miyu` |

formula 直接下载 Release 上的 `miyu-<版本>-<修订>-aarch64-apple-darwin.tar.gz`，把包里的
`bin/`、`share/` 原样放进 Cellar；依赖 `chafa`、`ripgrep`、`onnxruntime`（对照 Arch 配方：
alsa-lib / glibc / gcc-libs 在 macOS 上不适用，python 用 Command Line Tools 自带的 python3，
装 Homebrew 本来就要它）。formula 经 curl 下载不带隔离标记，所以不签名不公证；Apple Silicon
要求的签名由链接器自动加的 ad-hoc 签名满足，构建时会校验。包修订号 2 起对应 formula 的
`revision 1`，同版本重编时 brew 也会提示升级。

## Arch 的四份真相源

| 目录 | 用途 |
|---|---|
| `arch/miyu-git/` | AUR VCS 包，继续从 Git 源码构建；`pkgver()` 读取实际版本和提交 |
| `arch/miyu-release/` | 从普通版本 tag，或明确声明的补丁提交，构建主包/voice 拆包 |
| `arch/miyu/` | AUR 主包包装器，下载已发布的 Arch 主包后完整重包 |
| `arch/miyu-voice/` | AUR voice 包装器，依赖与下载资产一致的主包精确版本 |

源构建的 `package()` 调用源码内 `ci/lib/arch_package.py source-install`，直接使用
`common/assets.json`。因此配方只需 Git 源码及其声明的 source 文件，Python 已列入
makedepends，不依赖执行机的 CI 绝对路径。VCS 的远端源码必须包含这套资源清单和脚本后
才能发布对应 AUR 配方；本轮不会自动推送 AUR。

`miyu-release` 默认构建 `v0.6.0`，Wiki 固定为
`af99c4ac22a1be849206639807577a51b9e12061`。本地安装验收使用冻结源码快照的原生构建，
不需要先创建公开 tag。若发布资产确实使用 tag 以外的补丁，必须明确提供
`MIYU_RELEASE_SOURCE_COMMIT`、`MIYU_RELEASE_PATCH_REASON`，并记录到 release input；
Wiki 覆盖同样必须是完整提交哈希。禁止偷偷取远端 main 作为普通 release 来源。

二进制 AUR 配方已同步公开的 0.6.0-2 与真实 SHA256。后续新包产生且通过验收后，
渠道更新步骤才写入新的 `pkgver`、`pkgrel`、`_release_pkgrel` 和真实资产 SHA256。
不能预填 0.6.0 的假哈希，也不能把公开二进制资产校验改成 `SKIP`。
包装器复制整个 `usr/`，因此保留 `miyupm` 别名、字体、资源、许可证和未来新增文件。

## 原生 Arch 构建与打包

`linux/builders/Dockerfile.arch` 固定基础镜像 digest、Arch Archive 2026/09/13
软件源和 Rust 1.96.1。builder 准备依赖，源码构建与 makepkg 阶段以非 root 用户执行。
生成包时禁止网络，并使用 `makepkg --nodeps`，不会在打包阶段安装依赖。

`ci/lib/arch_package.py::package_arch` 接受已验证的 release input、asset、stage、
逐文件 inventory、明确输出路径和已准备的 builder image。它拒绝变化的目录清单，
校验产物 `.PKGINFO`，保存 makepkg/namcap 日志，并清理本次临时容器和目录。
Arch 主包依赖系统 `onnxruntime` provider，不携带 GNU 渠道的私有 ORT。
voice 精确依赖同次主包的 `版本-包修订号`。

验收在容器中完成：生成 `.SRCINFO`，检查 namcap 报告，`pacman -U` 安装主包与
voice，核对包内资源清单、自报版本与真实模型输出。测试包不可安装到执行机的生产 Arch。
所有实测证据保存在忽略提交的 `out/distribution/`。

## 安装资源

资源搜索顺序为显式 override、原本支持的用户覆盖（models）、安装前缀下的
`share/miyu`、Linux 系统 fallback、仅 debug 的源码 fallback。release 不读当前目录
伪造的 `src/memes` 或 `assets/models`。所有相对路径均相对安装 prefix；Arch 为 `/usr`，Homebrew 为 keg（`/opt/homebrew/Cellar/miyu/<版本>`，经 `/opt/homebrew/bin` 的链接启动时同样能找到）。

| 路径 | 内容 | 缺失时的行为 |
|---|---|---|
| `bin/miyu`、`bin/miyupm` | 主程序及别名 | 主程序不可用 |
| `bin/miyu-voice` | 可选语音前端 | 语音不可用 |
| `share/miyu/fonts/` | Noto CJK、Noto Emoji、JetBrains Mono；发布资产已包含 | 渲染可能退回文本 |
| `share/miyu/models/<id>/` | 本地 embedding 模型及 tokenizer | 语义检索退回关键词 |
| `share/miyu/memes/` | 内置表情库 | 内置表情不可用 |
| `share/miyu/personas/<人格>/scripts/` | 出厂脚本（09-23 起；老位置 `share/miyu/scripts/personas/<人格>/` 仍会被扫描，新包不再安装） | 对应脚本不可用 |
| `share/miyu/personas/<人格>/skills/<技能名>/` | 内置技能（`SKILL.md` 与它带路的 `scripts/`，09-23 起读盘加载） | 对应技能静默消失，技能脚本调不到 |
| `share/miyu/default-kb/` | 项目 kb、固定 Wiki 和来源 manifest | 默认知识库不可用 |
| `share/licenses/miyu*/` | 项目、字体、模型及语音依赖许可证 | 包验收失败 |

GNU 渠道还在 `lib/miyu/` 携带固定 CPU ORT；Arch 继续使用系统 provider。
