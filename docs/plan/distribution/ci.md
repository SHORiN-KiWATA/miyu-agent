# Linux CI 工作流

范围为 `linux-smoke` 发布目标。四份工作流和受控封装已实现。

**2026-09-21：`ci.yml` 第一次在 GitHub runner 上真跑并通过**（`b023e22a`，四个作业全绿：format-and-python 0.3 分钟、actionlint 0.1 分钟、MSRV 1.89.0 2.4 分钟、Rust 1.96.1 含 voice 面编译与 source-unit 6.7 分钟）。在此之前它从没在 runner 上跑成功过，连跑四次全断在第一步，后面的作业一次都没执行到——两处真问题见下。`release.yml` / `build-package-verify.yml` / `packaging-update.yml` 仍未在远端执行过：0.6.1 是本机跑的发布链。此文不把远端未执行的构建、安装或发布记为通过，也不宣称 macOS 已支持。

## 2026-09-20/21（0.6.1 发版时）改的七处

发 0.6.1 时把本机那条链整条跑了一遍，撞出来的都记在这儿：

1. **`release.yml` 的 `notes-path` 默认值会过期**。它指着 `docs/releases/0.6.0/release-notes.md`，版本升了没人改它，远端一执行就去找一个不存在的文件。现已改 0.6.1，并加 `packaging/ci/tests/test_workflows.py` 钉住：默认值必须跟 `Cargo.toml` 的版本走，且那一版的 `release-notes.md` 与 `changelog.md` 都真的存在。
2. **`MIYU_LANG: zh` 是必需的，不是偏好**。界面文案按 locale 走，`i18n.rs` 的 `system_with` 兜底是 `En`，而一批用例断言的正是中文那份（「限流」「已思考」「运行命令」）。runner 上 `LANG` 通常是 `C.UTF-8` → 解析成 En → 这些用例整批红。本机在 systemd 的 `en_US.UTF-8` 环境下实测 12 条红、换 `LANG=zh_CN.UTF-8` 后 2621 条全绿。`MIYU_LANG` 不依赖系统装没装 zh_CN 语言包，所以钉它而不是 `LANG`。⚠️ **光在作业里设还不够**：产品测试跑在 `Sandbox` 的干净环境里，环境变量过一张白名单，`MIYU_LANG` 原来不在表上，于是设了也到不了测试进程——0921 那次 CI 就是这么红的（runner 上 11 条 TUI 用例失败，本机 `env -u LANG` 完全复现）。白名单已放行，`refactor-check.sh` 也 `export MIYU_LANG=zh`，两边同源。真正的修法是让那些用例自己钉死 locale，未做。
3. **`.dockerignore`**（新增）。两个 builder 镜像一行 `COPY` 都没有，构建上下文应当是空的；仓库一直没有这份清单，于是手册里那条 `docker build … .` 会把整个仓库塞给守护进程——开发机 `target/` 实测 136 GB。`workflow.py build` 用冻结快照当上下文所以不受影响，但自托管 runner（`vars.LINUX_X64_RUNNER`）的工作区是热的，一样会踩。同一份测试也钉住「Dockerfile 不许开始依赖上下文」。
4. **新增 `workflows` 作业跑 actionlint**（1.7.12，下载后校验 SHA256 才执行），以及把「模型可见面无 CJK」门禁搬进 `format-and-python`——它原来只在本机 `refactor-check.sh` 里跑。
5. **`build-package-verify.yml` 超时 210 → 240 分钟**：`gnu-x86_64` 现在要装五个发行版（多了 Linux Mint 22.3）。

6. **新增 `cargo check --features voice --all-targets`**（1.96.1 那档）。默认 feature 是空的，所以这一面平时一行都不编——0.6.1 打包时才发现 `src/bin/voice.rs` 自 09-16 拆 crate 起就编不过（`miyu::voice::*` 早就搬进 `miyu-engine` 了）。sherpa 的静态库由 build.rs 现下。

仍然缺的（登记在案，不谎报）：`ci.yml` 没有任何 cargo 缓存，每次都是冷编译（本机实测冷构建光依赖就 40 分钟，`vars.LINUX_X64_RUNNER` 指自托管 runner 时尤其值得加）；`refactor-check.sh` 里的行数、层序两道门禁和 `cargo test --workspace` 仍只在本机跑（远端只有 `--suite source-unit`，即根 crate 的 lib 测试）。0.6.1 的本机实测：`cargo test --workspace` 2621 条用例、约 13 个测试目标，远端要跑得先解决缓存与时长。

## 2026-09-23：macOS 包（Homebrew 渠道）

用户拍板 macOS 包在 GitHub Actions 的 macos-15 runner 上构建，经 Homebrew tap
（`SHORiN-KiWATA/homebrew-miyu`）分发。新工作流 `macos-package.yml` 只管 macOS 这一份包，
发版链其余部分仍在本机（手册第 3b 节）：

- **触发靠推候选分支**：`release/v<版本>-<修订>`（release 模式）或 `macos-preview/**`（预览，
  不打 tag）。`workflow_dispatch` 只认默认分支上已有的工作流文件，发版时不能依赖它；分支名
  由 `workflow.py macos-params` 解析并校验。
- **同一份冻结输入**：runner 从同一个提交重新跑 `metadata.py --profile smoke`，本机用
  `workflow.py macos-import` 核对包记录与报告绑定的 `release-input.json` 与本机逐字节相同。
- **原生构建**（`lib/native_build.py`）：锁文件里的 Xcode 与 SDK、锁定的 Rust、离线 vendor
  （相对路径 `../inputs/vendor`，让构建记录与机器无关）；构建记录用宿主记录（系统 / Xcode /
  SDK / clang）代替镜像 digest；构建目录不许在家目录下，二进制里不许出现家目录路径；产物必须是
  arm64、最低系统 15.0、签名有效。
- **验收**（`lib/native_verify.py`）：解压到带空格的临时前缀跑 `probes/installed.py`（加
  `--functional`：资源按前缀找得到、内置技能加载得出、出厂脚本跑得起来），再用本次包渲染的
  formula 真 `brew install` + `brew test`。后者会往 Homebrew 装依赖，只在一次性 runner 上跑
  （`--allow-homebrew-changes`），个人 Mac 上默认拒绝。
- **凭据**：只有验收那一步拿到 `OPENCODEGO_PROVIDER_CONFIG`（`test_workflows.py` 钉住）。
  release 模式缺它直接失败；预览模式缺它时 provider-live 记 SKIPPED，其余检查必须全过。

## 工作流职责

| 工作流 | 入口 | 实际职责 |
|---|---|---|
| `ci.yml` | PR、main push、手动 | Rust 1.96.1 fmt、Python packaging 单测、模型面无 CJK、隔离 source-unit（`MIYU_LANG=zh`）；另用 Rust 1.89.0 执行 `cargo check --locked --all-targets`；独立作业跑 actionlint |
| `release.yml` | 仅手动 | 默认只生成 `NOT_EXECUTED` dry-run 计划；显式执行才走准备、构建、安装、聚合、发布 |
| `build-package-verify.yml` | 受控 reusable workflow | 仅接受 `gnu-x86_64` 或 `arch-x86_64`，运行该构建对应的全部安装目标 |
| `packaging-update.yml` | 手动指定已完成 release run | 下载已聚合产物，在同一源码 ref 上生成 AUR PKGBUILD、.SRCINFO、Homebrew formula（产物里有 macOS 包时）和 patch；不 apply、不推送渠道仓库 |
| `macos-package.yml` | 推 `release/v*`、`macos-preview/**` 分支，或手动 | 在 macos-15 上重新冻结、原生构建 `macos-core`、解压验收 + Homebrew 真装；产物 `verified-results-macos-arm64` 由本机导入聚合 |

Rust job 的两档工具链最多同时运行两个任务，且在格式/Python 检查通过后启动。发布构建矩阵也是 `max-parallel: 2`，GNU 与 Arch 各一条；各自的 core、voice 顺序构建。MSRV job 不使用 `continue-on-error`，当前依赖若不支持 1.89.0 会明确失败。

源码测试使用 `packaging/ci/run_tests.py --suite source-unit` 的归属标记和独立 HOME/MIYU_HOME/XDG、进程回收规则。PR 工作流不引用模型凭据。

## 运行主机与构建基线

2026-09-14 核对 `actions/runner-images` 官方 README 时，`ubuntu-26.04` 标记为 preview，`ubuntu-24.04` 为稳定的 `ubuntu-latest`。所以工作流采用 `${{ vars.LINUX_X64_RUNNER || 'ubuntu-24.04' }}`；管理员可设置该变量，但 runner 必须是具备 Docker 的原生 Linux x86_64。

host runner 的版本不决定 GNU ABI。GNU 构建仍使用源码中的 `packaging/linux/builders/Dockerfile.gnu` 及其 Debian 13 digest；Arch 使用固定快照 Dockerfile。封装创建镜像后解析实际 image ID，传给 build/package，并在使用完后删除这一个镜像，不清理用户其他镜像或容器。

Python 固定 3.11，发布 Rust 固定 1.96.1，MSRV 为 1.89.0。Cargo vendor 在准备 job 一次完成；两个 builder 消费同一快照。产品编译由现有 `build.py` 在 `--network none` 的容器内执行。

## 发布默认行为与执行条件

`release.yml` 没有 tag push 触发器。`workflow_dispatch.dry-run` 默认 `true`，只生成可下载的 `dry-run-plan.json`。它明确写 `status: NOT_EXECUTED`，不会调用 provider、伪造报告或调用 publish。

显式关闭 dry-run 后：

1. 检查专用 provider secret。
2. 要求输入 tag 恰为 `v<Cargo.toml version>`，这个已存在 tag、checkout HEAD 与控制工作流的 commit 必须一致；正整数 revision 经 argparse/metadata 校验。
3. `metadata.py --mode release --profile linux-smoke` 冻结源码，再完整 `prepare.py`。
4. GNU/Arch 原生容器构建 core/voice，stage 后打包。
5. Arch 跑 `arch-x86_64`；GNU 跑 Debian 13、Ubuntu 24.04、Ubuntu 26.04、Linux Mint 22.3、冻结 Fedora 版本这五个安装目标。每个目标都实际调用 `verify.py`，使用专用 opencodego/deepseek-v4.1-flash 配置。GNU tar 的验收由 manifest 固定到 Debian 目标，不能漏掉。
6. `verify_release.py` 必须收到全部真实 PASS 报告，覆盖 final package hash；随后才能调用 `publish.py --dry-run` 检查公开附件名单，只列 Arch、DEB、RPM 主包与 voice 包，共六个。
7. 单独 publish job 下载这份已校验 bundle，调用 `publish.py --execute`。唯一拥有 `contents: write` 的 job 是 publish。所有其他 job 默认只有 contents read；渠道 patch job 额外需要 actions read，以读取指定 run 的 artifact。

`notes-path` 默认 `docs/releases/0.6.0/release-notes.md`。执行发布时，这个文件必须已经纳入所选源码提交且非空。路径必须位于 checkout 内。第一次正式远端运行仍需实际验证 runner 容量、上游源可达性和专用凭据；本地 actionlint 不能证明这些条件。

`verified-publish` 是完整验证 bundle，不等于 GitHub Release 附件清单。GNU tar、截图、
SHA256SUMS、验收 JSON、SBOM、provenance 和输入/输出 manifest 保留在 bundle/CI artifact
中。发布器先完整校验所有这些证据，再只上传冻结资产中格式为 `archlinux`、`deb`、`rpm`
的六个包。远端已有额外附件（包括这些内部文件）仍会拒绝发布，不会自动删除或覆盖。
发布说明中的 OOBE 图片使用仓库资源链接，不依赖 Release 图片附件。

## 专用凭据契约

只有显式执行发布的准备预检和安装验证步骤读取 `OPENCODEGO_PROVIDER_CONFIG`。reusable workflow 通过同名 secret 显式传参，不使用 `secrets: inherit`。构建、打包、聚合不接收该凭据。

secret 必须是只含一个 `providers` 数组的 JSON，并且数组里恰好一个 `id: opencodego` 的 provider。需要非空 `api_key`，`models` 包含 `deepseek-v4.1-flash`；可保留 `display_name`、`base_url`、`protocol`。使用专用测试凭据，不复制完整生产配置。

wrapper 只保留上述 provider 字段，在输出树以外创建 0600 临时配置；验证结束后 finally 删除。artifact 只包含源码输入、包或已脱敏报告，从不打包这个临时配置。凭据缺失返回 exit 3，不能放行非 dry-run 发布。provider 调用成功与否仍由真实安装验收报告决定。

## 跨 job 文件传输

源码快照和 prepared inputs 放在一个 gzip tar 中传递。结果包/报告、聚合发布目录也分别使用 tar，GitHub artifact 上传关闭二次压缩。不能直接 upload 源码目录，让 artifact 服务把执行位归一化。

wrapper 解包先调用安全 tar 检查，拒绝绝对路径、`..`、逃逸链接、链接环和非空目的目录，再恢复 tar 中普通文件的精确权限，拒绝特殊权限位。这一步有实际依据：已准备的 `fnv-1.0.7` vendor 里有六个 0640 文件，直接用上游资源解包器归一成 0644 会使 `verify_prepared` 失败。已经实测 0640、0755、相对符号链接的归档往返均保持不变。

恢复后 build/stage 仍验证 source snapshot、manifest SHA 和 prepared 全量文件库存。传输归档不是额外信任来源。发布只接受两个固定 build ID 的结果，重复 asset/report 目录会失败。失败日志另打包到 `failed-results-*`，不混入可发布结果。

渠道更新需要在对应 release source tag/ref 上 dispatch。封装会准备一个 Arch 镜像，给 `channel_update.py --builder-image <实际 image ID>` 使用，避免依赖本机才有的镜像名；可选 `published-url` 触发正式 Release 回读校验。流程不会调用 `--apply` 或任何 AUR push。

## Actions 固定值与真实检查

下列值来自 GitHub API 对官方 repository tag 的实际解析，工作流全部使用完整 commit SHA：

| Action | 查询 tag | 已固定 commit |
|---|---|---|
| actions/checkout | v4 | `11d5960a326750d5838078e36cf38b85af677262` |
| actions/setup-python | v5 | `a26af69be951a213d495a4c3e4e4022e16d87065` |
| actions/upload-artifact | v4 | `ea165f8d65b6e75b540449e92b4886f43607fa02` |
| actions/download-artifact | v4 | `d3f86a106a0bac45b974a628896c90dbdf5c8093` |

实际下载并运行 actionlint 1.7.12，校验其官方 release checksums，Linux amd64 归档 SHA256 为 `8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8`。四份工作流实际通过 actionlint，exit 0。

本地实际通过的检查：

- metadata、prepare、build、stage、package、verify、verify_release、publish、run_tests、workflow、channel_update 共 11 个脚本的 `--help`。
- workflow 全部 10 个子命令的 `--help`。
- 默认 release plan 输出 `NOT_EXECUTED`，不调用构建、模型或发布。
- 缺专用 provider secret 时 exit 3。
- 0640/0755/相对链接保持，以及非空解包目录拒绝。
- 官方 Actions SHA 查询、runner 标签核对和 actionlint 1.7.12。

取证位于 `out/distribution/ci-check/`：`action-pins.json`、`runner-images-readme.md`、`script-interface-checks.json`、`workflow-static-checks.json`、`actionlint.txt`。测试用临时目录已自动清理；actionlint 临时二进制和下载归档在最终检查完成后删除，保留版本/来源/校验记录。

未执行：GitHub 远端 workflow、远端 MSRV 构建、远端五发行版 provider 验收、远端发布和渠道 push。本次没有操作远端。用户当前批准的本地 0.6.0 安装/发布链由主任务独立执行。
