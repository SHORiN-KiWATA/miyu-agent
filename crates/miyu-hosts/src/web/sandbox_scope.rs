//! 回合作用域(09-11 成员,09-13 起管理员 `/sandbox`):工作区落在哪、子进程套不套
//! Landlock。三处作用域化点(回合、重做、工具桥)都从 [`session_scope`] 拿,不各写一遍。
//!
//! - 会话归成员(归属键非空、账号不是管理员)→ 工作区 = `home/<用户>/workspace`
//!   (不看会话记录——成员改不了,也不该把 daemon 的 cwd 当工作区),成员策略
//!   (09-11 用户拍板:沙盒外的读取也禁):可写 {工作区, /tmp, /dev/null, 脚本缓存};
//!   只读只给跑程序必需的系统目录(/usr /etc /proc …)、内置与已装脚本目录、miyu
//!   自己的二进制;管理员的家、`~/.miyu` 的配置与库都摸不到。
//! - 管理员会话绑了沙盒根(`/sandbox <路径>`,会话记录 `sandbox`)→ 工作区 = 根,
//!   管理员策略:同样读写都锁,只比成员多配置里的工具链清单(`tools.sandbox`)与
//!   Miyu 自己的产出目录(artifact 库、生图/深研落盘)。绑的时候给了
//!   `--allow-read`(会话记录 `sandbox_read_all`)就只锁写:读放开成整个文件系统。
//! - 只读模式(09-23,Tab 切换,会话记录 `sandbox_readonly`)→ 压过上面两条:读全盘、
//!   哪儿都不许写(例外见 [`readonly_scope`])。工作目录照旧:绑了根就是根,跟着
//!   默认沙盒就是默认根,都没有就是客户端 cwd。
//! - 没绑、也没对这个会话说过不要沙盒(`sandbox_opt_out`),而全局「默认开启沙盒
//!   模式」开着(09-23)→ 读全盘、只能写默认根:在项目目录里打开的终端就是那个
//!   目录,家目录与没有当前目录的入口(WebUI、QQ、语音)是属主家里的 `workspace`
//!   (见 [`pick_default_root`])。
//! - 其余(管理员没绑、终端、平台回合)→ 客户端 cwd,否则 daemon cwd,不套沙盒。

use crate::web::*;
use miyu_base::sandbox::{LiveSandbox, SandboxPolicy, SandboxSource};

pub(in crate::web) struct TurnScope {
    pub(in crate::web) workspace: PathBuf,
    pub(in crate::web) policy: Option<Arc<SandboxPolicy>>,
}

/// 跑程序必需的系统目录:两种策略共用,只读 + 可执行。
const SYSTEM_READ_ONLY: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/proc", "/sys", "/dev", "/run", "/opt",
    "/var",
];

/// 工具链直通:清单里放行了真家的这些目录,就把对应变量指过去(HOME 已换成沙盒根,
/// 不指的话 cargo/rustup/npm/git 会到根下面找,要么重下要么找不到工具链)。
const TOOLCHAIN_ENV: &[(&str, &str)] = &[
    (".cargo", "CARGO_HOME"),
    (".rustup", "RUSTUP_HOME"),
    (".npm", "npm_config_cache"),
    (".gitconfig", "GIT_CONFIG_GLOBAL"),
];

/// 放行了才补进 PATH 头部的用户 bin 目录。
const PATH_PREPEND: &[&str] = &[".cargo/bin", ".local/bin"];

pub(in crate::web) fn session_scope(
    paths: &MiyuPaths,
    admin_store: &StateStore,
    stores: &StoreRegistry,
    config: &AppConfig,
    session_id: &str,
    client_cwd: Option<PathBuf>,
) -> TurnScope {
    if let Some(scope) = member_scope(paths, admin_store, stores, session_id) {
        return scope;
    }
    let record = stores
        .for_session(session_id)
        .session_record(session_id)
        .ok()
        .flatten();
    let bound = record.as_ref().and_then(|record| {
        let root = PathBuf::from(record.sandbox.clone()?);
        root.is_dir().then_some((root, record.sandbox_read_all))
    });
    let opted_out = record.as_ref().is_some_and(|record| record.sandbox_opt_out);
    let readonly = record
        .as_ref()
        .is_some_and(|record| record.sandbox_readonly);
    // 回合带着客户端目录来(REPL、shellhook、一次性 CLI);工具桥、重做、`/sandbox`
    // 查看不带,用这个会话上一回合的——不然 claude-code 经桥回调 Miyu 工具时关在
    // 工作区、它自己的进程却关在项目目录,两边对不上。
    let client_cwd = client_cwd.or_else(|| last_client_cwd(session_id));
    let platform = stores
        .for_session(session_id)
        .is_platform_session(session_id)
        .unwrap_or(false);
    let default_root = (bound.is_none() && !opted_out && config.tools.sandbox.default_enabled)
        .then(|| default_sandbox_root(paths, client_cwd.as_deref(), platform))
        .flatten();
    let client_workdir = || {
        client_cwd
            .clone()
            .filter(|path| path.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    };
    if readonly {
        let workdir = bound
            .as_ref()
            .map(|(root, _)| root.clone())
            .or_else(|| default_root.clone())
            .unwrap_or_else(client_workdir);
        return readonly_scope(paths, workdir);
    }
    if let Some((root, read_all)) = bound {
        return admin_scope(paths, config, root, read_all);
    }
    if let Some(root) = default_root {
        return admin_scope(paths, config, root, true);
    }
    TurnScope {
        workspace: client_workdir(),
        policy: None,
    }
}

/// 回合用的活沙盒(09-23 用户拍板:回合进行中按 Tab 切只读,下一次工具调用就
/// 生效)。策略每次重算都走 [`session_scope`],跟回合开始时同一条路;工作目录在
/// 回合开始时定下,整轮不变。
struct SessionSandboxSource {
    paths: MiyuPaths,
    admin_store: StateStore,
    stores: StoreRegistry,
    config: AppConfig,
    session_id: String,
    client_cwd: Option<PathBuf>,
}

impl SessionSandboxSource {
    fn scope(&self) -> TurnScope {
        session_scope(
            &self.paths,
            &self.admin_store,
            &self.stores,
            &self.config,
            &self.session_id,
            self.client_cwd.clone(),
        )
    }
}

impl SandboxSource for SessionSandboxSource {
    fn resolve(&self) -> Option<Arc<SandboxPolicy>> {
        self.scope().policy
    }
}

/// 回合的工作目录 + 活沙盒(回合与重做用;工具桥单次调用照旧用 [`session_scope`])。
pub(in crate::web) fn live_session_scope(
    paths: &MiyuPaths,
    admin_store: &StateStore,
    stores: &StoreRegistry,
    config: &AppConfig,
    session_id: &str,
    client_cwd: Option<PathBuf>,
) -> (PathBuf, Arc<LiveSandbox>) {
    let source = SessionSandboxSource {
        paths: paths.clone(),
        admin_store: admin_store.clone(),
        stores: stores.clone(),
        config: config.clone(),
        session_id: session_id.to_string(),
        client_cwd,
    };
    let workspace = source.scope().workspace;
    (workspace, Arc::new(LiveSandbox::new(Box::new(source))))
}

/// 每个会话上一回合的客户端目录(只在这个 daemon 进程里记,重启后等下一回合)。
fn client_cwds() -> &'static std::sync::Mutex<HashMap<String, PathBuf>> {
    static CWDS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PathBuf>>> =
        std::sync::OnceLock::new();
    CWDS.get_or_init(Default::default)
}

/// 回合开始时记下客户端目录(为什么要记见 [`session_scope`])。
pub(in crate::web) fn remember_client_cwd(session_id: &str, cwd: &std::path::Path) {
    if let Ok(mut cwds) = client_cwds().lock() {
        cwds.insert(session_id.to_string(), cwd.to_path_buf());
    }
}

fn last_client_cwd(session_id: &str) -> Option<PathBuf> {
    client_cwds().lock().ok()?.get(session_id).cloned()
}

/// 默认沙盒的根该落在哪(用户 09-23 拍板「自动检测」)。
///
/// 在项目目录里打开的终端,写的就是那个目录——不然默认沙盒一开,进了项目反而
/// 改不了项目,还得手动 `/sandbox` 一次。以下几种退回属主家里的 `workspace`:
/// - 没有当前目录的入口(WebUI、语音)与通讯平台(平台会话共用一个目录;图片这类
///   共通插件由 daemon 自己落盘,不受沙盒影响,用户 09-23);
/// - `/`、家目录本身,以及家目录与 `~/.miyu` 的**上级**——绑上去等于整个家可写;
/// - `~/.miyu` 里面(配置里的 key、daemon 正在写的库、会被 shell 在沙盒外执行的
///   hook)与家目录下的隐藏目录(`~/.ssh`、`~/.config`……)。
pub(in crate::web) fn pick_default_root(
    workspace: &std::path::Path,
    miyu_root: &std::path::Path,
    home: Option<&std::path::Path>,
    cwd: Option<&std::path::Path>,
    platform: bool,
) -> PathBuf {
    let usable = |cwd: &std::path::Path| {
        if cwd.parent().is_none() || miyu_root.starts_with(cwd) || cwd.starts_with(miyu_root) {
            return false;
        }
        let Some(home) = home else {
            return true;
        };
        if home.starts_with(cwd) {
            return false;
        }
        match cwd.strip_prefix(home) {
            Ok(rest) => !rest
                .components()
                .next()
                .is_some_and(|first| first.as_os_str().to_string_lossy().starts_with('.')),
            Err(_) => true,
        }
    };
    cwd.filter(|_| !platform)
        .and_then(|cwd| cwd.canonicalize().ok())
        .filter(|cwd| cwd.is_dir() && usable(cwd))
        .unwrap_or_else(|| workspace.to_path_buf())
}

/// 全局「默认开启沙盒模式」给没说过要不要沙盒的会话用的根(怎么挑见
/// [`pick_default_root`])。工作区是属主家里的 `workspace`,不是 `~/.miyu` 本身;
/// 老布局(没有 `home/`)退回 `~/.miyu/workspace`。
///
/// 这台机器没有沙盒后端就不套(警告一次):默认开着却让每条命令都失败关闭,比
/// 不开更糟。用户显式 `/sandbox`、切只读仍照旧当场报错。
pub(in crate::web) fn default_sandbox_root(
    paths: &MiyuPaths,
    client_cwd: Option<&std::path::Path>,
    platform: bool,
) -> Option<PathBuf> {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if miyu_base::sandbox::probe().is_none() {
        if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                "default sandbox is on but this system has no sandbox backend; not confining"
            );
        }
        return None;
    }
    let workspace = paths.default_sandbox_dir();
    let miyu_root = paths
        .root_dir
        .canonicalize()
        .unwrap_or_else(|_| paths.root_dir.clone());
    let home = directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .map(|home| home.canonicalize().unwrap_or(home));
    let root = pick_default_root(
        &workspace,
        &miyu_root,
        home.as_deref(),
        client_cwd,
        platform,
    );
    if root != workspace {
        return Some(root);
    }
    if let Err(error) = miyu_base::paths::ensure_private_dir(&workspace) {
        tracing::warn!(error = %error, path = %workspace.display(), "default sandbox workspace dir");
        return None;
    }
    Some(workspace)
}

/// 只读模式(09-23,Tab):读全盘、哪儿都不许写,`/tmp` 也不例外(用户拍板「只读就
/// 该彻底只读」)。只管磁盘上的文件:记忆、todo、知识库、生图/搜图落盘这些在
/// daemon 里写,不受影响。例外只留不给就跑不起来的:`/dev/null`(`2>/dev/null`
/// 都会报错)、运行时 socket(MCP 桥连回 daemon);中转线 CLI 自己的配置目录由
/// `confine_relay` 另外放行,不给就起不来。代价:要临时文件的命令(部分编译、
/// `mktemp`、存登录态的脚本)在只读下会失败。HOME 不换、不设工具链直通。
pub(in crate::web) fn readonly_scope(paths: &MiyuPaths, workdir: PathBuf) -> TurnScope {
    let mut read_write = vec![PathBuf::from("/dev/null"), paths.runtime_dir()];
    read_write.retain(|path| path.exists());
    let policy = SandboxPolicy {
        root: workdir.clone(),
        read_only: vec![PathBuf::from("/")],
        read_write,
        home: None,
        env: Vec::new(),
        path_prepend: Vec::new(),
        writable_summary: vec!["nothing".to_string()],
        readable_summary: vec!["everything".to_string()],
        read_only_mode: true,
    };
    TurnScope {
        workspace: workdir,
        policy: Some(Arc::new(policy)),
    }
}

/// 会话归不归成员(归属键非空、账号不是管理员)。成员的沙盒定死在自己家里:
/// `/sandbox`、只读开关都不归他们管。
pub(in crate::web) fn member_owns_session(
    admin_store: &StateStore,
    stores: &StoreRegistry,
    session_id: &str,
) -> bool {
    stores.owner_of_session(session_id).is_some_and(|owner| {
        !owner.is_empty()
            && admin_store
                .account_by_id(&owner)
                .ok()
                .flatten()
                .is_some_and(|account| !account.is_admin())
    })
}

fn system_read_only(paths: &MiyuPaths) -> Vec<PathBuf> {
    let mut read_only: Vec<PathBuf> = SYSTEM_READ_ONLY.iter().map(PathBuf::from).collect();
    read_only.push(paths.scripts_dir.clone());
    read_only.push(paths.system_scripts_dir.clone());
    // 人格资源树(出厂脚本、技能与技能带路的脚本,09-23 起都住这儿):不放行的话
    // 沙盒会话读不到技能、也执行不了出厂脚本。装在 `/usr`、`/opt` 下的本来就在
    // 上面那张系统表里;`~/.local` 前缀与开发树不在,得点名。
    read_only.extend(paths.system_personas_dirs());
    // 用 miyu_executable()(剥掉 `/proc/self/exe` 的「 (deleted)」后缀)而不是裸
    // current_exe():部署/重建把二进制换掉后,运行中 daemon 的 current_exe() 读成
    // `.../miyu (deleted)`,那条会被下面 retain(exists) 剔掉→沙盒不放行真二进制的
    // EXECUTE;而 CLI 后端(claude-code 等)起的 `miyu mcp-serve` 用的正是剥过后缀
    // 的真路径,exec 被 Landlock 挡下→claude 报 CONNECTION_CLOSED、MCP 用不了
    // (09-12 坐实的 MCP 桥连不上真凶)。两处取同一条路径。
    if let Ok(exe) = miyu_base::paths::miyu_executable() {
        read_only.push(exe);
    }
    read_only
}

/// daemon 的运行时目录(IPC socket core.sock 在里面):沙盒会话用 claude-code 等
/// CLI 后端时,CLI 起的 `miyu mcp-serve` 桥要连这个 socket 把工具调用转回 daemon
/// 才拿得到 Miyu 工具。CLI 进程被 Landlock 关着,桥子进程继承规则,不放行这条就
/// 连不上、报 CONNECTION_CLOSED(09-11 实测)。桥转的工具调用带会话、在 daemon 侧
/// 按会话作用域执行,不越权;裸 IPC 的特权命令(Shutdown 等)按「防君子不防小人」
/// 的既定尺度不设防(Landlock 本就不管 socket)。runtime 目录只含 miyu 自己的
/// 运行时文件,给读写(connect 需要)。
fn base_read_write(paths: &MiyuPaths, root: &std::path::Path) -> Vec<PathBuf> {
    let mut read_write = vec![
        root.to_path_buf(),
        PathBuf::from("/tmp"),
        PathBuf::from("/dev/null"),
        paths.cache_dir.clone(),
    ];
    let runtime_dir = paths.runtime_dir();
    if runtime_dir.exists() {
        read_write.push(runtime_dir);
    }
    read_write
}

fn member_scope(
    paths: &MiyuPaths,
    admin_store: &StateStore,
    stores: &StoreRegistry,
    session_id: &str,
) -> Option<TurnScope> {
    let owner = stores.owner_of_session(session_id)?;
    if owner.is_empty() {
        return None;
    }
    let account = admin_store.account_by_id(&owner).ok().flatten()?;
    if account.is_admin() {
        return None;
    }
    let home = paths.user_home_dir(&account.username);
    let workspace = home.join("workspace");
    if let Err(error) = miyu_base::paths::ensure_private_dir(&workspace) {
        tracing::warn!(error = %error, path = %workspace.display(), "member workspace dir");
    }
    // 成员自己的产出目录(artifact 库、生图落盘)也得能读写——artifact 落在
    // `home/<user>/artifacts`(见 tools::artifact::artifacts_root),沙盒不放行就
    // 会「读 artifact:x 报 outside your workspace」(09-11 实测)。先建出来,
    // Landlock 对不存在的授权根是失败关闭。
    let artifacts = home.join("artifacts");
    if let Err(error) = miyu_base::paths::ensure_private_dir(&artifacts) {
        tracing::warn!(error = %error, path = %artifacts.display(), "member artifacts dir");
    }
    let mut read_only = system_read_only(paths);
    // 成员自己家里的只读产出目录:文档、图片(vision/print_image 读得到自己
    // 生成的图)。会话库、profile 这些不放行,「沙盒外读取也禁」的口径不变。
    read_only.push(home.join("documents"));
    read_only.push(home.join("pictures"));
    // 成员私有人格的脚本/技能就在 `home/<user>/personas/<人格>/` 下(见
    // web::member_persona)。read_only 给的是 FS_EXECUTE|FS_READ:不放行这条,成员
    // 注册的脚本一调用就被 Landlock 挡在 exec 上(「脚本一调用就被拒」的真凶)。
    // 是成员自己家里的东西,只读执行不越权。
    read_only.push(home.join("personas"));
    // Landlock 对打不开的授权根是失败关闭:不存在的目录先剔掉。
    read_only.retain(|path| path.exists());
    let mut read_write = base_read_write(paths, &workspace);
    read_write.push(artifacts);
    read_write.retain(|path| path.exists());
    let policy = SandboxPolicy {
        root: workspace.clone(),
        read_only,
        read_write,
        home: Some(workspace.clone()),
        env: Vec::new(),
        path_prepend: Vec::new(),
        writable_summary: vec!["root".to_string(), "/tmp".to_string()],
        readable_summary: vec![
            "root".to_string(),
            "/tmp".to_string(),
            "system dirs".to_string(),
        ],
        read_only_mode: false,
    };
    Some(TurnScope {
        workspace,
        policy: Some(Arc::new(policy)),
    })
}

/// 管理员 `/sandbox <root>` 的策略。`/sandbox` 查看也走这里,所以摘要里列的就是
/// 真正装进规则集的东西(清单里不存在的路径不会出现)。
///
/// `read_all` = `--allow-read`:读放开成整个文件系统,写侧一个字不动。
pub(in crate::web) fn admin_scope(
    paths: &MiyuPaths,
    config: &AppConfig,
    root: PathBuf,
    read_all: bool,
) -> TurnScope {
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    let expand = |value: &str| -> Option<PathBuf> {
        let value = value.trim();
        if let Some(rest) = value.strip_prefix("~/") {
            return home.as_ref().map(|home| home.join(rest));
        }
        let path = std::path::Path::new(value);
        path.is_absolute().then(|| path.to_path_buf())
    };
    let abbreviate = |path: &std::path::Path| -> String {
        match home.as_ref().and_then(|home| path.strip_prefix(home).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    };

    let mut read_only = system_read_only(paths);
    let mut readable_summary = vec![
        "root".to_string(),
        "/tmp".to_string(),
        "system dirs".to_string(),
    ];
    for entry in &config.tools.sandbox.readable {
        if let Some(path) = expand(entry).filter(|path| path.exists()) {
            readable_summary.push(abbreviate(&path));
            read_only.push(path);
        }
    }
    read_only.retain(|path| path.exists());

    let mut read_write = base_read_write(paths, &root);
    // Miyu 自己经工具产出的目录:artifact 库(artifact 工具写、read artifact: 读)、
    // 生图/深研落盘(print_image/vision 回读自己生成的图)。成员版少放行一条就
    // 「读 artifact:x 报 outside your workspace」,这里一次放齐。不存在的不建,
    // 由各工具自己按需建;建出来之前那一轮读不到,下一轮策略重算就有了。
    read_write.push(paths.artifacts_dir());
    read_write.push(paths.documents_dir());
    read_write.push(paths.pictures_dir.clone());
    let mut writable_summary = vec!["root".to_string(), "/tmp".to_string()];
    for entry in &config.tools.sandbox.writable {
        if let Some(path) = expand(entry).filter(|path| path.exists()) {
            writable_summary.push(abbreviate(&path));
            read_write.push(path);
        }
    }
    read_write.retain(|path| path.exists());

    // 工具链直通按**显式**清单判,所以在读放开之前留一份:不然下面那条 `/`
    // 会让 granted() 恒真,CARGO_HOME 这类变量指到一个只读的目录上(cargo 拿不到
    // 锁,报错比不设更难懂)。放不放行工具链仍只看 `tools.sandbox` 两份清单。
    let toolchain_read = read_only.clone();
    if read_all {
        // Landlock 是 allow-list:`/` 上一条读+执行的规则就覆盖全盘,写侧不受影响
        // (写只认 read_write)。`guard_read` 判的是同一个列表,进程内工具跟着放开
        // ——两层一处开关。系统目录等条目被它整个包住,不必再逐条装。
        read_only = vec![PathBuf::from("/")];
        readable_summary = vec!["everything (read-only)".to_string()];
    }

    let granted = |path: &std::path::Path| {
        toolchain_read
            .iter()
            .chain(read_write.iter())
            .any(|allowed| path.starts_with(allowed))
    };
    let mut env = Vec::new();
    let mut path_prepend = Vec::new();
    if let Some(home) = &home {
        for (suffix, key) in TOOLCHAIN_ENV {
            let path = home.join(suffix);
            if path.exists() && granted(&path) {
                env.push((key.to_string(), path.display().to_string()));
            }
        }
        for suffix in PATH_PREPEND {
            let path = home.join(suffix);
            if path.is_dir() && granted(&path) {
                path_prepend.push(path);
            }
        }
    }
    let policy = SandboxPolicy {
        root: root.clone(),
        read_only,
        read_write,
        home: Some(root.clone()),
        env,
        path_prepend,
        writable_summary,
        readable_summary,
        read_only_mode: false,
    };
    TurnScope {
        workspace: root,
        policy: Some(Arc::new(policy)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认沙盒的根怎么挑(用户 09-23「自动检测」):项目目录就是它;家目录本身、`/`、
    /// 它们与 `~/.miyu` 的上级、`~/.miyu` 里面、家下的隐藏目录、没有当前目录的入口、
    /// 通讯平台,一律退回工作区。
    #[test]
    fn default_root_follows_the_project_directory_but_not_sensitive_places() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let miyu = home.join(".miyu");
        let workspace = miyu.join("home/me/workspace");
        let project = home.join("code/proj");
        let hidden = home.join(".ssh");
        let elsewhere = root.join("srv/app");
        for dir in [&workspace, &project, &hidden, &elsewhere] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let pick = |cwd: Option<&std::path::Path>, platform| {
            pick_default_root(&workspace, &miyu, Some(&home), cwd, platform)
        };
        assert_eq!(pick(Some(&project), false), project, "家里的项目目录");
        assert_eq!(pick(Some(&elsewhere), false), elsewhere, "家外面的目录");
        assert_eq!(pick(Some(&home), false), workspace, "家目录本身");
        assert_eq!(pick(Some(&root), false), workspace, "家目录的上级");
        assert_eq!(pick(Some(std::path::Path::new("/")), false), workspace, "/");
        assert_eq!(pick(Some(&workspace), false), workspace, "~/.miyu 里面");
        assert_eq!(pick(Some(&hidden), false), workspace, "家下的隐藏目录");
        assert_eq!(pick(None, false), workspace, "没有当前目录的入口");
        assert_eq!(pick(Some(&project), true), workspace, "通讯平台");
    }

    /// 人格资源树(技能、出厂脚本)在沙盒里要读得到——`~/.local` 前缀与开发树
    /// 不在系统表里,只放行老的 `scripts/` 的话,09-23 搬家后技能与出厂脚本在
    /// 沙盒会话里全部失效。
    #[test]
    fn the_persona_resource_tree_is_readable_in_the_sandbox() {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = MiyuPaths::new().unwrap();
        paths.system_scripts_dir = temp.path().join("scripts");
        let personas = paths.system_personas_dir().unwrap();
        assert!(system_read_only(&paths).contains(&personas));
    }
}
