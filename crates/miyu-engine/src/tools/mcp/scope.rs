//! 起一个服务器进程时要带上的调用方环境：沙盒策略、工作目录、会话（09-25）。
//!
//! 这几样都是调用方任务上的 task-local，只在调用方的任务里看得见。进程在 MCP 线程上起
//! （`runtime`），所以在调用时取下来带过去。原来服务器进程从不进沙盒、跑在 daemon 的
//! 目录里：成员会话能借它碰到沙盒外的东西。

use miyu_base::config::{McpSandbox, McpServerConfig};
use miyu_base::sandbox::SandboxPolicy;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct SpawnScope {
    pub(super) sandbox: Option<Arc<SandboxPolicy>>,
    pub(super) workdir: Option<PathBuf>,
    pub(super) session: Option<Arc<str>>,
}

impl SpawnScope {
    /// 调用方此刻的环境。
    pub(super) fn capture() -> Self {
        Self {
            sandbox: miyu_base::sandbox::current_sandbox(),
            workdir: miyu_base::workspace::try_workspace(),
            session: miyu_base::workspace::try_session(),
        }
    }

    /// 列举工具时用：只是握手、问一句工具清单，不带会话，照旧在 daemon 的环境里起。
    pub(super) fn listing() -> Self {
        Self {
            sandbox: None,
            workdir: None,
            session: None,
        }
    }

    /// 这个服务器实际要装的策略。`sandbox = "none"` 只对属主生效：成员会话的策略照装。
    pub(super) fn policy_for(&self, server: &McpServerConfig) -> Option<Arc<SandboxPolicy>> {
        let policy = self.sandbox.clone()?;
        if server.sandbox == McpSandbox::None && !policy.member {
            return None;
        }
        Some(policy)
    }

    /// 能不能共用一个进程的凭据里，调用方这一侧的部分：装的策略、工作目录、会话。
    /// 策略起进程时装上就撤不掉（agy 进程池同一条），所以整份算进来。
    pub(super) fn fingerprint(&self, server: &McpServerConfig) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"session\0");
        hasher.update(self.session.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\0workdir\0");
        if let Some(workdir) = &self.workdir {
            hasher.update(workdir.display().to_string().as_bytes());
        }
        hasher.update(b"\0sandbox\0");
        if let Some(policy) = self.policy_for(server) {
            hasher.update(format!("{policy:?}").as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// `sandbox_writable` 里的路径：`~/` 开头按家目录展开。
pub(super) fn writable_paths(server: &McpServerConfig) -> Vec<PathBuf> {
    server
        .sandbox_writable
        .iter()
        .map(|raw| expand_home(raw.trim()))
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}
