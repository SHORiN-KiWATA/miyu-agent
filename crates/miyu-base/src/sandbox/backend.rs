//! Prepare backend selection before fork. Only syscall-safe work runs in apply.

use super::SandboxPolicy;

pub(super) enum Rules {
    #[cfg(target_os = "linux")]
    Linux(super::linux::Rules),
    #[cfg(target_os = "macos")]
    Macos(super::macos::Rules),
    #[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
    Unsupported,
}

#[cfg(test)]
tokio::task_local! {
    // Task-scoped fault injection exercises the real public confine entrypoints.
    // It cannot affect another test or change backend selection after fork.
    pub(super) static FORCE_UNSUPPORTED: bool;
}

impl Rules {
    pub(super) fn prepare(
        policy: &SandboxPolicy,
        command: &(std::ffi::OsString, Vec<std::ffi::OsString>),
    ) -> Self {
        #[cfg(test)]
        if FORCE_UNSUPPORTED
            .try_with(|forced| *forced)
            .unwrap_or(false)
        {
            return Self::Unsupported;
        }
        #[cfg(target_os = "linux")]
        {
            let _ = command;
            Self::Linux(super::linux::Rules::prepare(policy))
        }
        #[cfg(target_os = "macos")]
        {
            Self::Macos(super::macos::Rules::prepare(policy, &command.0, &command.1))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (policy, command);
            Self::Unsupported
        }
    }

    pub(super) fn apply(&self) -> std::io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Linux(rules) => rules.apply(),
            #[cfg(target_os = "macos")]
            Self::Macos(rules) => rules.apply(),
            #[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
            Self::Unsupported => super::unsupported::apply(),
        }
    }
}
