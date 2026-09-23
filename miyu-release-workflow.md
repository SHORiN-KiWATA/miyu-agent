# Miyu 发布流程

本手册按 2026-09-20 的 **0.6.1** 实际构建、容器验收、发布、AUR 推送与本机升级更新。工具入口在
`packaging/ci/`，资源清单在 `packaging/common/assets.json`，Arch 四份配方仍以
`packaging/arch/` 为真相源，Homebrew formula 与 tap 的 README 以 `packaging/homebrew/`
为真相源。远端 CI 的职责与凭据契约见
[CI 说明](docs/plan/distribution/ci.md)。

## 范围与工作区

`linux-smoke` 的目标：Arch、Debian 13、Ubuntu 24.04 LTS、Ubuntu 26.04、
Linux Mint 22.3、Fedora 44，均为 Linux x86_64。两个 Ubuntu 一头一尾：24.04 是声称
支持的**下限**，26.04 是最新。（0.6.0 当时验的是 25.10 而不是 24.04；2026-09-20 起
构建基座降到 24.04，往下多盖了两个 LTS 周期的用户。）Mint 22.x 的基座就是
Ubuntu 24.04，但它自带软件源，DEB 的依赖是从 Mint 自己的镜像拉的，所以单独装一次。公开附件仅为 Arch、DEB、RPM 的主程序与 voice，共六个包。GNU tar 仅保留内部验收。

**2026-09-23 起 profile 改用 `smoke`**：上面六个 Linux 目标原样不动，再加 `macos-arm64`
（只有主程序，没有 voice）。macOS 包是 Homebrew formula 下载的那个 tar.gz，所以它是
**第七个公开附件**；它在 GitHub Actions 的 macos-15 runner 上构建与验收（第 3b 节），本机
负责其余一切。只有 Linux 的远端 `release.yml` 仍用 `linux-smoke`。
主程序要求真实安装、资源/版本校验及指定供应商正常回复；voice 要求实际安装和版本校验。
GNU tar 使用独立安装前缀与 MIYU_HOME。物理麦克风、Mac 和完整升级恢复不在这次验收范围。

在独立 worktree 操作。是否合并 main、部署宿主、推送 AUR 或更新另一个软件源，取决于
当前任务的明确范围；这些都不是“创建 GitHub Release”自动执行的附带步骤。
任务已授权合并、push、AUR 或本机升级时，应连续完成并核对结果，不再重复等待验收。
保留其他工作区和 AUR 检出的未提交内容；用户要求清理时，只移除已确认由旧安装留下的 Miyu 覆盖文件。

## 1. 准备与源码门禁

版本同时写入 Cargo.toml、Cargo.lock 和源码发布配方。二进制 AUR 包装器保留上次公开
资产的真实版本/hash，等新资产发布并回读后再更新，避免提前发布不存在的下载地址。

源码通过 `cargo fmt --check`、隔离的 `test_scripts/refactor-check.sh`、声明 MSRV 的
`cargo check --locked --all-targets` 和 Python 打包测试。产品测试必须使用临时
HOME/MIYU_HOME/XDG；进程与目录清理由 `Sandbox`、`ProcessSupervisor` 管理。
`packaging/ci/run_tests.py` 提供预定义源码测试入口。

跑测试的环境必须给到中文 locale（`LANG=zh_CN.UTF-8`，或直接 `MIYU_LANG=zh`）。界面文案
按 locale 走、兜底是英文，而一批用例断言的正是中文那份——2026-09-20 在 `systemd --user`
的 `en_US.UTF-8` 环境里实测 12 条红，换上 locale 后 2621 条全绿。别把这类红当成回归。

手册里那条 `docker build … .` 依赖仓库根的 `.dockerignore`（内容是一个 `*`）：两个 builder
镜像一行 `COPY` 都没有，上下文应当是空的；少了那份清单，docker 会把整个工作区
（开发机 `target/` 实测 136 GB）当上下文收走。

CI 的本机开发依赖还必须包含 **ripgrep**；搜索测试会实际调用 `rg`，缺失即 ENOENT。
图片/表情包尺寸在 `cfg(test)` 下固定为 80×24，测试不得依赖 runner 是否有控制终端或 TERM。
生产终端探测保持真实值。测试回归应同时在无控制终端、删除 TERM/COLORTERM 的环境执行。

测试清理按本进程收养关系识别后代，排除启动前已有的子进程；不要读取可变或因
non-dumpable 而无法访问的 `/proc/<pid>/environ` 来证明归属。发出信号后必须 wait 确认回收。
命令原始退出码、tests.txt 与 cleanup_error 分别保留，清理失败不能掩盖源测试失败。

```bash
python3 -m unittest discover -s packaging/ci/tests -v
python3 packaging/ci/run_tests.py --suite source-unit --report-dir out/distribution/source-unit
```

先用 preview 输入完成真实安装排错。源码完成后提交 release commit，创建本地版本 tag。
正式输入要求源码 clean，tag、Cargo 版本与 source commit 一致。无需为了构建先合并 main
或提前公开未验收的 tag。

## 2. 冻结并准备最终输入

以下参数对应 0.6.1（revision 1）。每次构建使用新的空输出目录，不能覆盖上一轮证据。
metadata 同时保存源码快照和文件清单。运行前必须检出待发布源码提交；不要在后续
渠道/文档提交的 HEAD 上直接重跑旧版本 tag。新版本使用 revision 1，同版本重编见最后一节。

```bash
python3 packaging/ci/metadata.py --mode release --source-ref HEAD --tag v0.6.1 \
  --profile smoke --revision 1 --out out/distribution/release-0.6.1/release-input.json
python3 packaging/ci/prepare.py --manifest out/distribution/release-0.6.1/release-input.json \
  --out out/distribution/release-0.6.1/inputs
```

已验证的下载缓存和相同 Cargo.lock 的 vendor 可通过 `--cache`、`--vendor-cache` 复用；
`--offline` 要求缓存完整，否则失败。Wiki、模型、ORT、sherpa 与工具下载均受锁定 hash 校验。
GNU 构建镜像基于 Debian 13；Arch 使用锁定基础镜像和 Archive 2026/09/13 软件快照。
两个 Dockerfile 位于 `packaging/linux/builders/`。记录准备出的实际 image ID。

```bash
docker build -t miyu-release-gnu -f packaging/linux/builders/Dockerfile.gnu .
docker build -t miyu-release-arch -f packaging/linux/builders/Dockerfile.arch .
```

冷构建时依赖下载和最终链接可能数分钟不刷新日志。查看下载文件增长、容器状态与编译进程，
不能把“暂时没新日志”直接当成卡死。

## 3. 构建与打包

对 `gnu-x86_64`、`arch-x86_64` 各构建 core/voice。`build.py` 必须接收 manifest、inputs、
build-id、component、out、builder-image；可指定该 component 独占的 `--target-cache`。
容器内 `CARGO_BUILD_JOBS=2` 是冻结在源码快照里的（`ci/lib/container_build.py`），所以
并行度只看同时起几条。CI 上是两条（GNU 与 Arch 各一条，各自 core→voice 串行）；开发机
核多的时候可以把四条（两个 build-id × core/voice）一起起，4×2=8 核，墙钟时间对折——
0.6.1 就是这么构建的，`--target-cache` 按 component 分开所以不抢 Cargo 锁。
编译在 `--network none` 容器内以 `--release --frozen` 执行，
保留 thin LTO/codegen-units=1；链接几分钟属正常，不能因日志暂时不变就重启构建。

每个 build-id 依次调用 `stage.py` 和 `package.py`。stage 接收对应 `--build-root`；package
按 manifest 中的每个 `--asset-id` 生成到独立 `--out`。DEB/RPM 传 checksum 验证过的绝对
`--nfpm` 路径，Arch 传实际 `--builder-image`。各脚本 `--help` 为当前参数契约。

发布包必须包含字体、模型、表情、脚本、知识库和许可证。构建记录经 stage/package
绑定到实际二进制；不能只改 JSON 中的 source/hash 来复用旧二进制。Arch 使用系统 ORT，
GNU 使用私有 CPU ORT。Arch namcap E 会拒绝打包，RPM 不声明发行版共有目录的所有权。

## 3b. macOS 构建与验收（GitHub Actions，2026-09-23 起）

macOS 没有容器，包由 `.github/workflows/macos-package.yml` 在 macos-15 runner 上原生构建。
本机冻结的输入传不上去，所以 runner 从**同一个提交**再冻结一次，两边的 `release-input.json`
必须逐字节相同——元数据只取决于提交内容与锁文件，第 2 节那条命令在哪台机器上跑结果都一样。

1. 第 2 节冻结完，记下本机的哈希：`sha256sum out/distribution/release-<版本>/release-input.json`。
2. 把 release commit 推成候选分支（对外操作，按任务授权；tag 仍然等全部验收后才推）：

   ```bash
   git push origin <release-commit>:refs/heads/release/v<版本>-<修订>
   ```

   分支名就是发版请求：工作流从名字里取 tag 与修订号，在 runner 上补一个本地轻量 tag 再冻结。
   不用 `workflow_dispatch` 是因为它只认默认分支上已有的工作流文件。
3. 等它跑完（冷构建约半小时），核对 job summary 里的 `release-input.json sha256` 与第 1 步一致：

   ```bash
   gh run list --repo SHORiN-KiWATA/miyu-agent --workflow macos-package.yml --branch release/v<版本>-<修订>
   gh run watch <run-id> --repo SHORiN-KiWATA/miyu-agent
   ```

4. 下载并导入。`macos-import` 会再核对一遍包记录与报告绑定的是本机这份输入，然后放进
   `packages/macos-core` 与 `reports/macos-arm64`，之后照常走第 5 节的聚合：

   ```bash
   gh run download <run-id> --repo SHORiN-KiWATA/miyu-agent -n verified-results-macos-arm64 \
     -D out/distribution/release-<版本>/macos-download
   python3 packaging/ci/workflow.py macos-import \
     --archive out/distribution/release-<版本>/macos-download/results-macos-arm64.tar.gz \
     --manifest out/distribution/release-<版本>/release-input.json \
     --packages out/distribution/release-<版本>/packages --reports out/distribution/release-<版本>/reports
   ```

5. 正式发布、tag 推上去之后删掉候选分支：`git push origin --delete release/v<版本>-<修订>`。

runner 上做了什么：锁文件里的 Xcode（`sudo xcode-select`，SDK 对不上直接 BLOCKED）、锁定的 Rust、
离线 vendor；构建目录放在 `/tmp` 下并检查二进制里没有构建机的家目录（编译期的源码根路径会被
编进二进制）；产物必须是 arm64、最低系统 15.0、带有效签名。验收分两段：解压到带空格的临时
前缀跑安装探针（逐文件哈希、版本、`miyu paths` 的「系统人格资源目录」落在前缀里、内置技能
全部加载得出、出厂脚本跑得起来、真模型回一句），再用本次的包渲染 formula（file:// 地址）真
`brew install` + `brew test`，最后卸载并确认测试 tap 已移除。

真模型那一项要仓库的 Actions secret `OPENCODEGO_PROVIDER_CONFIG`（专用测试凭据，格式见
`docs/plan/distribution/ci.md`）。候选分支（release 模式）缺它直接失败；`macos-preview/**`
分支是预览模式，没有凭据时这一项记 SKIPPED，其余照验，产物只供调试，不能进发布。

## 4. 实际安装与模型验收

对 manifest 的六个 Linux target-id 分别运行 `verify.py`（`macos-arm64` 在 runner 上验，见第 3b 节）：

```bash
python3 packaging/ci/verify.py --manifest out/distribution/release-0.6.1/release-input.json \
  --packages out/distribution/release-0.6.1/packages --target-id debian13-x86_64 \
  --report-dir out/distribution/release-0.6.1/reports/debian13-x86_64 \
  --provider-config /home/shorin/.miyu/config/config.jsonc
```

这条本机配置路径仅用于本次用户已授权的本地验收。脚本只临时复制 opencodego provider，
调用 deepseek-v4.1-flash，不上传凭据。远端 Actions 另需专用测试 secret，不能把宿主配置
上传为 artifact。成功要求请求退出正常、最终回复非空、provider/model 正确，不要求人格
逐字照抄某个测试口令。

六个目标是 `arch-x86_64`、`debian13-x86_64`、`ubuntu2404-x86_64`、
`ubuntu2604-x86_64`、`mint22-x86_64`、`fedora-current-x86_64`。0.6.1 共 42 项必需
检查（六个目标 × 主包 4 项 + voice 2 项，外加 GNU tar 的 6 项），均需 PASS。`smoke` 再加
macOS 的 5 项（身份、安装、资源、真模型、Homebrew formula），共 47 项。
变更 CLI 默认行为时还要运行真实 PTY：本次确认不设置 MIYU_TUI 默认全屏，
MIYU_TUI=0 回退 inline。不能在验收命令里继续设置 MIYU_TUI=1 而把默认行为缺陷藏起来。

所有目标报告都必须指向最终包 hash。首次失败报告保留，重试用新目录。最终聚合目录只放
各目标适用的成功报告。容器必须确认已移除，才能删除 bind-mounted home 并宣布清理完成。
不要按日期猜测 Ubuntu 旧版本已经迁到 old-releases，保留能实际验证的官方源。

## 5. 聚合、上传与回读

```bash
python3 packaging/ci/verify_release.py --manifest out/distribution/release-0.6.1/release-input.json \
  --artifacts out/distribution/release-0.6.1/packages --reports out/distribution/release-0.6.1/reports \
  --publish-dir out/distribution/release-0.6.1/publish
python3 packaging/ci/publish.py --manifest out/distribution/release-0.6.1/release-input.json \
  --dir out/distribution/release-0.6.1/publish --dry-run
```

### 公开附件与内部证据

`verify_release.py` 生成并验证完整内部 bundle；`publish.py` 只从中选择
`archlinux`、`deb`、`rpm`。两者职责不同，不得遍历 publish 目录直接全部上传。

| 发行版 | 主包 | 语音包 |
| --- | --- | --- |
| Arch | `.pkg.tar.zst` | `.pkg.tar.zst` |
| Debian / Ubuntu（共用） | `.deb` | `.deb` |
| Fedora | `.rpm` | `.rpm` |
| macOS（Apple Silicon，Homebrew 下载） | `.tar.gz` | — |

公开附件固定为上述七个（`linux-smoke` 是前六个）。macOS 的 tar.gz 没有签名：formula 经 curl
下载不会被打隔离标记，浏览器直接下载则会被 Gatekeeper 拦，发布说明要写明 macOS 只支持
`brew install shorin-kiwata/miyu/miyu`。GNU tar、截图、SHA256SUMS、acceptance、SPDX、provenance、
release-input 与 release-manifest 都不上传 Release。GitHub 自动生成的两项 Source code
下载由平台提供，不计入手动附件。内部 bundle 及 CI artifact 仍保留完整证据。
SPDX 在当前工具中是文件清单，不等于完整依赖 SBOM；provenance 是构建来源，
release-input / release-manifest 分别记录输入与输出，不是供用户安装的文件。

### 发布说明与图片

先逐条核对 `next-release-note.md`，完整归档到 `docs/releases/<version>/changelog.md`。
正文按用户能感知的主题整理，并保留重要功能、修复、命令变化与升级注意事项，
不能只挑 OOBE 或 UI 而漏掉 CLI 后端、沙盒、压缩、记账等整块更新。

图片放在仓库 `docs/releases/<version>/`，使用 `raw.githubusercontent.com` 的 tag 或
确定提交链接嵌入正文。可以收折次要图片，保留真实截图，不将截图作为 Release 附件。
0.6.0-2 用过四张 OOBE 图和三张实机截图；0.6.1 的正文是纯文字（用户要求简短）。用图时发布前逐个读取图片 URL，核对返回内容与
仓库原图 hash；删除旧图片附件前先更新正文链接。全部最终包的 SHA256 放正文
`<details>` 折叠区，并链接完整 changelog。正文完成并确认没有遗漏后才清空已归档记录。

### 上传与回读

全部验证成功后推送已授权的分支和 tag，再把 dry-run 换为 `--execute`，同时传
`--notes docs/releases/0.6.1/release-notes.md`。首次上传创建 draft，每个包上传后下载
回读 hash，公开附件名单（`smoke` 七个）核对后才转正式。已有同名异内容或额外远端资产会失败，不能
使用 clobber 绕过；同版本替换按最后一节的单独迁移步骤处理。

## 6. 合并 main 并推送

先检查主工作区脏文件。待发布记录先备份并逐条确认已归档；用户 todolist 等无关改动
原样保留。可快进时使用 `git merge --ff-only <发布分支或提交>`，随后明确执行
`git push origin main`，并用 `git ls-remote origin refs/heads/main` 对照本地 HEAD。
本地合并或 commit 不能算完成 push。后续渠道/CI/文档修复也要推送，不能只推 release tag。

`miyu-git` 的远端 main 必须先包含其 PKGBUILD 调用的资源脚本，再更新 AUR VCS 配方。

## 7. 同步 AUR 与 Homebrew tap

正式 Release 回读成功后执行渠道生成，以下为本次参数形态：

```bash
python3 packaging/ci/channel_update.py \
  --manifest out/distribution/release-0.6.1/release-input.json \
  --release-output out/distribution/release-0.6.1/publish/release-manifest-0.6.1-1.json \
  --published-url https://github.com/SHORiN-KiWATA/miyu-agent/releases/tag/v0.6.1 \
  --out out/distribution/release-0.6.1/channels \
  --builder-image miyu-release-arch --apply
```

仓库 `packaging/arch/` 是真相源：`miyu` / `miyu-voice` 的 pkgver、pkgrel、
_release_pkgrel、URL、SHA256、精确主包依赖与 .SRCINFO 必须一致；`miyu-release`
源码配方也同步包修订。`miyu-git` 根据当前已推送源码版本、提交计数与短 SHA 更新
快照 pkgver，生成并维护其 .SRCINFO。VCS 的 pkgver() 在用户构建时继续计算实际源码版本。

从正式 URL 下载，用新配方在容器重包并安装，检查资源、版本、别名与真实模型回复。
0.6.0-2 的 AUR 渠道另外完成过 6 项检查，0.6.1 同法。

同步独立 AUR 检出前 fetch 并检查 HEAD/远端与脏文件。未跟踪的旧包、日志、pkg/src
不是未提交的配方修改，应保留；若配方本身有改动，先保留并合并，不能直接覆盖。
只复制、提交 PKGBUILD 与 .SRCINFO，分别 push miyu、miyu-voice、miyu-git 的实际分支。
最后对照 AUR 远端 HEAD，并确认这三个检出的两份文件与主仓逐字节一致。

### Homebrew tap（`smoke` 发布时）

同一条 `channel_update.py` 在发布产物里有 `macos-core` 时，会回读线上 macOS 包的哈希，
把地址、版本、哈希与 revision（包修订号 2 起写 `revision 1`，同版本重编也会让 brew 提示升级）
写进 `channels/homebrew/Formula/miyu.rb`；`--apply` 同时更新仓库里的真相源
`packaging/homebrew/Formula/miyu.rb`。`channels/homebrew/` 就是 tap 仓库该有的全部内容
（README 与 formula），整份复制过去：

```bash
git -C ~/Documents/github/homebrew-miyu pull --ff-only
cp -r out/distribution/release-<版本>/channels/homebrew/. ~/Documents/github/homebrew-miyu/
git -C ~/Documents/github/homebrew-miyu add README.md Formula/miyu.rb
git -C ~/Documents/github/homebrew-miyu commit -m "miyu <版本>"
git -C ~/Documents/github/homebrew-miyu push
```

推之前确认 `Formula/miyu.rb` 的 sha256 不是全零、与 Release 上那个 tar.gz 的哈希一致。
第一次发布前 tap 仓库还不存在：`gh repo create SHORiN-KiWATA/homebrew-miyu --public`
后再推（对外操作，先问）。仓库名必须带 `homebrew-` 前缀，用户才能写
`brew install shorin-kiwata/miyu/miyu`。Homebrew 6 起第三方 tap 要显式信任，全名安装只信任
这一个 formula，说明里一律写全名。第一个带 macOS 包的版本发布后，README 的「如何安装？」与
`docs/wiki/01-快速开始.md` 补上 macOS 段落（tap 上线前写进去，用户照着装会失败）。

macOS 段还要写明两件事（09-23 定）：macOS 版**不带语音**，用嘴代替打字用系统听写（连按两下
Fn）；想用 Siri 免手跟 Miyu 说话，就在「快捷指令」里串「听写 → 运行 shell 脚本 → 朗读」，
shell 里写绝对路径 `/opt/homebrew/bin/miyu ask "…"`（快捷指令的 shell 没有 Homebrew 的
PATH），回复里的 Markdown 符号 Siri 会照着念；据记忆还要在快捷指令设置 → 高级里打开「允许
运行脚本」，写之前在真 Mac 上核实。

## 8. 升级本机与清理覆盖文件（任务已授权时）

升级使用正式回读通过的包，不能把未验收候选装进生产机。先记录 pacman 版本、PATH
实际解析、8300 监听 PID、对应 exe、生产 MIYU_HOME 与服务归属；只轮换这个 home
的生产 daemon，保留其他工作区/测试 home 的进程。不要读取并打印完整环境或密钥。

```bash
sudo pacman -U --noconfirm \
  out/distribution/release-0.6.1/publish/miyu-0.6.1-1-x86_64.pkg.tar.zst \
  out/distribution/release-0.6.1/publish/miyu-voice-0.6.1-1-x86_64.pkg.tar.zst
MIYU_HOME="$HOME/.miyu" /usr/bin/miyu daemon stop
```

本机已有语音包时，应同时升级主包与语音包。用包管理器版本和安装后文件 hash 确认升级，
再移除用户已要求清理且确认为旧覆盖的 `~/.local/bin/miyu`。其他 `.local/bin` 工具保留。
Arch 的 `/usr/sbin` 与 `/usr/bin` 可能指向同一目录，以解析后的实际路径判断。

已有托管服务时复用其服务管理方式；未托管时可以用用户 systemd 启动，避免临时测试 shell
结束后带走生产 daemon。本次采用下面的临时用户服务，不额外设置开机自启：

```bash
systemd-run --user --collect --unit=miyu-daemon \
  --property=Restart=on-failure --working-directory="$HOME" \
  -E "MIYU_HOME=$HOME/.miyu" -E LANG=zh_CN.UTF-8 -E LANGUAGE=zh_CN:en \
  /usr/bin/miyu __daemon --port 8300
```

端口、语言与 MIYU_HOME 按实际部署保持原值。检查服务 active、监听 PID 的 exe 为包内
程序、PATH 不再命中旧覆盖、WebUI/既有平台连接恢复。版本/模型黑盒测试仍在隔离 home
执行，不向生产会话或 QQ 发送验收消息。已有终端会话需重新打开才使用新前端。

## 9. 清理与最终确认

记录本轮创建的目录、容器、镜像及原有资源基线。停止并确认本轮进程/容器消失后，再删
其临时 HOME/MIYU_HOME/XDG、准备输入、重复源码、stage、独立 Cargo target、下载缓存
与重包目录。只删除已登记的本轮镜像，禁止全局 Docker prune。用户原有工具链缓存、
既有镜像/卷、AUR 未跟踪文件与生产数据保留。活库备份使用 VACUUM INTO，禁止 fs::copy。

保留最终发行 bundle、原资产替换备份和必要日志/报告。最后核对：main 与远端一致，
release tag 对应真实构建源码，附件精确七个（`linux-smoke` 为六个）且 hash/正文/图片一致，
AUR 远端及配方一致，tap 的 `Formula/miyu.rb` 与仓库 `packaging/homebrew/` 逐字节一致，
本机版本及实际 exe 正确，测试资源无残留。源码/CI 修复必须有修前红测和修后绿测；
新 CI 若失败先读测试日志与清理记录，不以“只有清理失败”推断 Rust 已全过。

## 9b. 0.6.1 实录（2026-09-20/21）与量出来的数

| 环节 | 实测 |
| --- | --- |
| 冷构建（四条并行，容器内 jobs=2 各条） | 依赖阶段约 40 分钟 |
| 改一处源码后重构建（`--target-cache` 指回上一轮） | **core 9m06s / voice 5m12s**，只重编 5 个单元 |
| 六目标容器验收 | 约 12 分钟，42 项必需检查全 PASS |
| AUR 包装包重包 + 真装验收 | 6/6 PASS |
| 全量测试（`refactor-check.sh`） | 2621 条用例 |

**重构建为什么这么快**：vendor 出来的依赖是「registry 形态带 checksum」，cargo 不按
mtime 判它们，所以把 `--target-cache` 指回上一轮的缓存目录，只有 `/source` 里那
5 个 path crate 会重编。代价是那份 target 缓存约 8 GB——留着省半小时，删掉下一轮
从依赖重来。

这一轮撞到的四件事（都已经变成门禁或清单）：

1. **`--features voice` 那一面平时一行都不编**。默认 feature 是空的，`cargo check
   --workspace --all-targets` 与 `cargo test --workspace` 都不碰它，只有打包那一步会
   真去构建 `--bin miyu-voice --features voice`。0.6.1 构建当天才发现 `src/bin/voice.rs`
   自 09-16 拆 crate 起就编不过。`refactor-check.sh` 与 `ci.yml` 已各加一步
   `cargo check --features voice --all-targets`。
2. **测试环境必须给中文 locale**（见第 1 节）。
3. **`.SRCINFO` 不要手改**，用 `makepkg --printsrcinfo` 生成——这次正是靠它发现
   `miyu-git/.SRCINFO` 少了 09-18 给 PKGBUILD 加的两条 `optdepends`。
   `channel_update.py` 只管 `miyu` / `miyu-voice` 两个包装包，`miyu-git` 的快照 pkgver
   （`<版本>.r<提交数>.g<短 SHA>`，按**已推送的 main**算）要自己更新。
4. **新发行版目标先用旧包探一次依赖**：`docker run <镜像> apt-get install --dry-run
   <上一版的 deb>` 就能看出除 glibc 之外的依赖在那家源里解不解得开。Mint 22.3 这次
   的输出正好是「只有 `libc6 (>= 2.41) but 2.39` 不满足」，于是新基座这一版必然能装。

**发布说明里的 SHA256 与 tag 的先后**：hash 只能在包构建出来之后才有，而包的字节
又包含正文所在的源码快照。所以 tag 停在被构建的那个提交，校验值随后单独提交进
main（0.6.0 与 0.6.1 都是这么做的）——不要为了把 hash 塞进 tag 而重编。

**清理**：删本轮 builder 镜像（两个共约 5.7 GB，从固定 Dockerfile 重建即可）、
`inputs`/`source`/`ci-*`/重包工作目录；保留 `publish`（最终 bundle）、`reports`、
`release-input.json` 与各步日志。按 digest 固定的六个安装镜像（Mint 那个 2.96 GB
最大）留着下一轮直接用，要腾空间就单独 `docker rmi`。别做全局 prune——本机有别人
的长期容器在跑。⚠️ 门禁跑完 `target/` 会长一大截（这次 136 → 169 GB），那是主检出
的编译缓存，不属于本轮产物，别顺手 `cargo clean`。

## 10. 同版本重编（仅在用户明确要求替换发布包时）

默认应发布新的应用补丁版本。用户明确要求同版本重编时，递增 package revision，
重新提交源代码、构建全部包并执行真实安装验收。保留旧 tag commit 与原资产本地备份，
发布说明写明重编原因、修订号和新源码。验证成功后才更新版本 tag（用旧远端值作为
force-with-lease 条件），使自动源码下载对应实际构建源码；不得伪改构建记录复用旧二进制。

先上传并回读新修订的全部公开包，再移除旧包和内部附件，最后验证远端精确附件名单。
替换期间暂停正常发布器的“远端不得有额外资产”步骤；这是人工执行的有旧新资产
清单及 hash 校验的迁移，正常发布器继续拒绝冲突与额外资产。之后用正常发布器再次
核验最终状态。按新 hash 更新仓库/AUR 配方、Homebrew formula（包修订号变了会多一行 `revision`）和正文 SHA256，不能沿用旧校验值。


0.6.0-2 的实际顺序是：保存旧 release body / asset IDs / tag 对象 → 本地更新 tag 并
冻结新源码 → 四个二进制重编、五目标验收 → 推送源码 main → 用旧 tag **对象 ID**
作 force-with-lease 条件推送 tag → 上传并回读六个新包（当时还没有 macOS 包）→ 验证仓库图片并更新正文 →
按已保存的旧 asset ID 删除旧附件 → 正常发布器再验精确六附件 → 更新 AUR、升级本机。
删除时拒绝未在原清单中的并发新增资产；重试时同名新包先比 hash，不重复覆盖。

仅渠道、测试夹具或文档的后续修正不改变已发布二进制的源码身份，不能因此悄悄把
release tag 移到后续 HEAD。0.6.0-2 二进制对应 9142225c，后续 CI 修复单独记录在 main。
