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
//! - 其余(管理员没绑、终端、平台回合)→ 客户端 cwd,否则 daemon cwd,不套沙盒。

use crate::web::*;
use miyu_base::sandbox::SandboxPolicy;

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
    let bound = stores
        .for_session(session_id)
        .session_record(session_id)
        .ok()
        .flatten()
        .and_then(|record| {
            let root = PathBuf::from(record.sandbox?);
            root.is_dir().then_some((root, record.sandbox_read_all))
        });
    if let Some((root, read_all)) = bound {
        return admin_scope(paths, config, root, read_all);
    }
    let workspace = client_cwd
        .filter(|path| path.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    TurnScope {
        workspace,
        policy: None,
    }
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
    };
    TurnScope {
        workspace: root,
        policy: Some(Arc::new(policy)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
