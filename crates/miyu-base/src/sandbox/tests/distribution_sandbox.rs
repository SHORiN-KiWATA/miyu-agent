use super::super::*;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
enum Entry {
    Async,
    Std,
    Relay,
}

const ENTRIES: [Entry; 3] = [Entry::Async, Entry::Std, Entry::Relay];

/// 把「这台机器上没有可用后端」这个现场造出来。
///
/// 有后端的平台只能靠注入(`FORCE_UNSUPPORTED` 是 task-local,只影响这一个任务)。
/// macOS 原来落在下面那一支——它当时**真的**没有后端,`probe()` 就是 None;
/// 接上 sandbox-exec 之后 `probe()` 返回 `Some(1)`,那句断言当场就炸(09-23 CI)。
async fn unavailable_backend<F: std::future::Future>(future: F) -> F::Output {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        backend::FORCE_UNSUPPORTED.scope(true, future).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        assert_eq!(probe(), None);
        future.await
    }
}

// All paths are arguments, never interpolated shell code.
async fn sentinel(entry: Entry, target: &Path) -> std::io::Result<std::process::ExitStatus> {
    let script = "printf sentinel > \"$1\"";
    match entry {
        Entry::Std => {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", script, "sandbox-test"]).arg(target);
            confine_std(&mut command);
            command.status()
        }
        Entry::Async | Entry::Relay => {
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", script, "sandbox-test"]).arg(target);
            match entry {
                Entry::Async => confine(&mut command),
                Entry::Relay => confine_relay(&mut command, &[]),
                Entry::Std => unreachable!(),
            }
            command.status().await
        }
    }
}

#[tokio::test]
async fn unsupported_policy_refuses_all_entrypoints_before_sentinel() {
    assert_eq!(unsupported::probe(), None);
    let temp = tempfile::tempdir().unwrap();
    let policy = Arc::new(SandboxPolicy {
        root: temp.path().to_path_buf(),
        read_only: vec![PathBuf::from("/")],
        read_write: vec![temp.path().to_path_buf()],
        ..Default::default()
    });
    unavailable_backend(with_sandbox(Some(policy), async {
        for entry in ENTRIES {
            let target = temp.path().join(format!("{entry:?}"));
            let result = sentinel(entry, &target).await;
            assert!(
                !target.exists(),
                "{entry:?} executed with an unsupported policy"
            );
            assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOTSUP));
        }
    }))
    .await;
}

#[tokio::test]
async fn unsupported_without_policy_runs_all_entrypoints() {
    let temp = tempfile::tempdir().unwrap();
    unavailable_backend(with_sandbox(None, async {
        for entry in ENTRIES {
            let target = temp.path().join(format!("{entry:?}"));
            assert!(sentinel(entry, &target).await.unwrap().success());
            assert_eq!(std::fs::read_to_string(target).unwrap(), "sentinel");
        }
    }))
    .await;
}

#[tokio::test]
async fn guards_follow_external_symlinks_and_nonexistent_leaves() {
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("allowed");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, allowed.join("escape")).unwrap();
    let policy = Arc::new(SandboxPolicy {
        root: allowed.clone(),
        read_write: vec![allowed.clone()],
        ..Default::default()
    });
    with_sandbox(Some(policy), async {
        assert!(guard_write(&allowed.join("missing/leaf")).is_ok());
        assert!(guard_read(&allowed.join("escape/missing/leaf")).is_err());
        assert!(guard_write(&allowed.join("escape/missing/leaf")).is_err());
        assert!(guard_write(&allowed.join("../outside/new")).is_err());
    })
    .await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_all_entrypoints_enforce_policy_and_inherited_grants() {
    let abi = probe().expect("BLOCKED: Linux kernel without Landlock");
    eprintln!("distribution_sandbox Linux kernel Landlock ABI: {abi}");
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("allowed");
    let readonly = temp.path().join("readonly");
    let outside = temp.path().join("outside");
    for path in [&allowed, &readonly, &outside] {
        std::fs::create_dir_all(path).unwrap();
    }
    let policy = Arc::new(SandboxPolicy {
        root: allowed.clone(),
        read_only: vec![PathBuf::from("/")],
        read_write: vec![allowed.clone()],
        home: Some(allowed.clone()),
        ..Default::default()
    });
    with_sandbox(Some(policy), async {
        for entry in ENTRIES {
            assert!(sentinel(entry, &allowed.join(format!("{entry:?}")))
                .await
                .unwrap()
                .success());
            for denied in [&readonly, &outside] {
                let path = denied.join(format!("{entry:?}"));
                assert!(!sentinel(entry, &path).await.unwrap().success());
                assert!(!path.exists());
            }
        }
        let mut command = tokio::process::Command::new("sh");
        command
            .args([
                "-c",
                "sh -c 'printf forbidden > \"$1\"' nested \"$1\"",
                "sandbox-test",
            ])
            .arg(outside.join("grandchild"));
        confine(&mut command);
        assert!(!command.status().await.unwrap().success());
        assert!(!outside.join("grandchild").exists());
    })
    .await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_relay_preserves_home_and_grants_only_explicit_config() {
    let abi = probe().expect("BLOCKED: Linux kernel without Landlock");
    eprintln!("distribution_sandbox Linux kernel Landlock ABI: {abi}");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let config = temp.path().join("config");
    let sibling = temp.path().join("sibling");
    for path in [&root, &config, &sibling] {
        std::fs::create_dir_all(path).unwrap();
    }
    let original_home = std::env::var_os("HOME").unwrap_or_default();
    let policy = Arc::new(SandboxPolicy {
        root: root.clone(),
        home: Some(root.clone()),
        read_only: vec![PathBuf::from("/")],
        read_write: vec![root.clone()],
        ..Default::default()
    });
    with_sandbox(Some(policy), async {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "test \"$HOME\" = \"$1\" && printf allowed > \"$2\" && ! sh -c 'printf forbidden > \"$1\"' child \"$3\"", "sandbox-test"])
            .arg(original_home).arg(config.join("sentinel")).arg(sibling.join("sentinel"));
        confine_relay(&mut command, std::slice::from_ref(&config));
        assert!(command.status().await.unwrap().success());
        assert!(config.join("sentinel").is_file());
        assert!(!sibling.join("sentinel").exists());

        let mut ordinary = tokio::process::Command::new("sh");
        ordinary.args(["-c", "test \"$HOME\" = \"$1\"", "sandbox-test"]).arg(&root);
        confine(&mut ordinary);
        assert!(ordinary.status().await.unwrap().success());
    }).await;
}
