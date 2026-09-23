use super::*;

/// 只读根 + 可写 /tmp 目录:目录里能写,别处不能;规则随 exec 继承到 sh。
///
/// 09-23 起 macOS 也跑这一条。**这是沙盒唯一的行为级证据**——「编过」不等于
/// 「关得住」，而 macOS 那一版走的是完全不同的机制（execv 到 sandbox-exec，
/// 见 `sandbox::macos`）。不在两个平台都跑，改动就只有编译保证。
///
/// 读那一侧两边不同：Linux 的 Landlock 是白名单，没列的路径读也拒（所以策略里
/// 给了 `read_only: ["/"]`）；macOS 那一版只收写，读本来就是放开的。这条脚本
/// 末尾读 `/etc/hosts`（两个平台都有）——在 Linux 上验的是「放行的读得到」，
/// 在 macOS 上验的是「读没被误伤」。
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn member_policy_confines_shell_writes() {
    let abi = probe().expect("BLOCKED: no sandbox backend on this machine");
    eprintln!("sandbox backend ready: {abi}");
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("allowed");
    std::fs::create_dir_all(&allowed).unwrap();
    let denied = temp.path().join("denied");
    std::fs::create_dir_all(&denied).unwrap();
    let policy = Arc::new(SandboxPolicy {
        root: allowed.clone(),
        read_only: vec![PathBuf::from("/")],
        read_write: vec![allowed.clone(), PathBuf::from("/dev/null")],
        home: Some(allowed.clone()),
        ..Default::default()
    });
    let script = format!(
        "echo ok > {}/a.txt && ! (echo no > {}/b.txt) 2>/dev/null && cat /etc/hosts >/dev/null",
        allowed.display(),
        denied.display()
    );
    let status = with_sandbox(Some(policy), async move {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(script);
        confine(&mut command);
        command.status().await.unwrap()
    })
    .await;
    assert!(status.success(), "sandboxed shell script failed: {status}");
    assert!(allowed.join("a.txt").is_file());
    assert!(!denied.join("b.txt").exists());
}

/// 只读模式(09-23)的形状:根是工作目录,但不在可写清单里——写进根必须失败,
/// 放行的临时目录能写,全盘能读。macOS 那一版以前会把 `root` 隐式放开,这条在
/// 两个平台都跑,守的就是那个口子。
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn readonly_policy_keeps_the_root_itself_unwritable() {
    probe().expect("BLOCKED: no sandbox backend on this machine");
    let temp = tempfile::tempdir().unwrap();
    let workdir = temp.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    let scratch = temp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let policy = Arc::new(SandboxPolicy {
        root: workdir.clone(),
        read_only: vec![PathBuf::from("/")],
        read_write: vec![scratch.clone(), PathBuf::from("/dev/null")],
        read_only_mode: true,
        ..Default::default()
    });
    let script = format!(
        "! (echo no > {}/a.txt) 2>/dev/null && echo ok > {}/b.txt && cat /etc/hosts >/dev/null",
        workdir.display(),
        scratch.display()
    );
    let status = with_sandbox(Some(policy), async move {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(script);
        confine(&mut command);
        command.status().await.unwrap()
    })
    .await;
    assert!(status.success(), "sandboxed shell script failed: {status}");
    assert!(!workdir.join("a.txt").exists());
    assert!(scratch.join("b.txt").is_file());
}

/// 只读模式下进程内写被拒时,报错说的是「只读模式开着」而不是「出了工作区」——
/// 后者会让模型去换个目录再试。
#[tokio::test]
async fn readonly_mode_says_so_when_a_write_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let policy = Arc::new(SandboxPolicy {
        root: temp.path().to_path_buf(),
        read_only: vec![PathBuf::from("/")],
        read_only_mode: true,
        ..Default::default()
    });
    let target = temp.path().join("a.txt");
    let (write, read) = with_sandbox(Some(policy), async {
        (guard_write(&target), guard_read(&target))
    })
    .await;
    let error = write.expect_err("writes are refused").to_string();
    assert!(error.contains("read-only mode is on"), "{error}");
    assert!(read.is_ok());
}

/// 活沙盒(09-23):版本号不变就不去重算(不是每次工具调用都读库),有人改过设置
/// 下一次取就是新的——回合进行中按 Tab 切只读靠的就是这个。
#[tokio::test]
async fn live_sandbox_recomputes_only_after_an_epoch_bump() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Source {
        calls: Arc<AtomicUsize>,
        readonly: Arc<AtomicBool>,
    }
    impl SandboxSource for Source {
        fn resolve(&self) -> Option<Arc<SandboxPolicy>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(SandboxPolicy {
                read_only_mode: self.readonly.load(Ordering::SeqCst),
                ..Default::default()
            }))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let readonly = Arc::new(AtomicBool::new(false));
    let live = Arc::new(LiveSandbox::new(Box::new(Source {
        calls: calls.clone(),
        readonly: readonly.clone(),
    })));
    let (before, after) = with_live_sandbox(live, async {
        let before = (0..3)
            .map(|_| current_sandbox().unwrap().read_only_mode)
            .collect::<Vec<_>>();
        let resolves_before = calls.load(Ordering::SeqCst);
        readonly.store(true, Ordering::SeqCst);
        bump_sandbox_epoch();
        let after = current_sandbox().unwrap().read_only_mode;
        (
            (before, resolves_before),
            (after, calls.load(Ordering::SeqCst)),
        )
    })
    .await;
    assert_eq!(before, (vec![false, false, false], 1), "没改过就不重算");
    assert_eq!(after, (true, 2), "改过之后下一次取就是新的");
}

/// 进程内守卫:可写根里能读能写,只读根里只能读,别处都不行;`..` 绕不出去。
#[tokio::test]
async fn in_process_guard_follows_the_policy() {
    let temp = tempfile::tempdir().unwrap();
    let rw = temp.path().join("rw");
    let ro = temp.path().join("ro");
    std::fs::create_dir_all(&rw).unwrap();
    std::fs::create_dir_all(&ro).unwrap();
    std::fs::write(ro.join("a.txt"), "a").unwrap();
    let policy = Arc::new(SandboxPolicy {
        root: rw.clone(),
        read_only: vec![ro.clone()],
        read_write: vec![rw.clone()],
        ..Default::default()
    });
    let outside = temp.path().join("outside.txt");
    let outside_in = outside.clone();
    with_sandbox(Some(policy), async move {
        let outside = outside_in;
        assert!(guard_read(&ro.join("a.txt")).is_ok());
        assert!(guard_write(&ro.join("a.txt")).is_err());
        assert!(guard_read(&rw.join("new.txt")).is_ok());
        assert!(guard_write(&rw.join("new.txt")).is_ok());
        assert!(guard_read(&outside).is_err());
        assert!(guard_write(&rw.join("../outside.txt")).is_err());
        assert!(guard_read(std::path::Path::new("/etc/hostname")).is_err());
    })
    .await;
    assert!(guard_read(&outside).is_ok(), "no policy = no guard");
}

/// `--allow-read` 的策略形状(只读根 = `/`):读哪儿都行,写仍然只在根里。
/// 守卫与 Landlock 吃的是同一个列表,所以这里两层一起验。
#[cfg(target_os = "linux")]
#[tokio::test]
async fn allow_read_policy_locks_writes_only() {
    probe().expect("BLOCKED: kernel without Landlock");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "OUTSIDE-MARKER").unwrap();
    let policy = Arc::new(SandboxPolicy {
        root: root.clone(),
        read_only: vec![PathBuf::from("/")],
        read_write: vec![root.clone(), PathBuf::from("/dev/null")],
        home: Some(root.clone()),
        ..Default::default()
    });
    let script = format!(
        "cat {}/secret.txt && echo ok > {}/a.txt && ! (echo no > {}/b.txt) 2>/dev/null",
        outside.display(),
        root.display(),
        outside.display()
    );
    let (outside_in, root_in) = (outside.clone(), root.clone());
    let (status, stdout) = with_sandbox(Some(policy), async move {
        // 进程内守卫:根外读得到,根外写不了。
        assert!(guard_read(&outside_in.join("secret.txt")).is_ok());
        assert!(guard_read(std::path::Path::new("/etc/hostname")).is_ok());
        assert!(guard_write(&outside_in.join("b.txt")).is_err());
        assert!(guard_write(&root_in.join("a.txt")).is_ok());
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(script);
        confine(&mut command);
        let output = command.output().await.unwrap();
        (
            output.status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    })
    .await;
    assert!(status.success(), "sandboxed shell script failed: {status}");
    assert!(
        stdout.contains("OUTSIDE-MARKER"),
        "root-outside read: {stdout}"
    );
    assert!(!outside.join("b.txt").exists(), "write escaped the root");
}

/// 工具链直通:HOME 换根、策略里的环境变量透传、PATH 头部补目录。
#[tokio::test]
async fn child_env_carries_home_toolchain_and_path() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let policy = SandboxPolicy {
        root: root.clone(),
        home: Some(root.clone()),
        env: vec![("CARGO_HOME".to_string(), "/real/.cargo".to_string())],
        path_prepend: vec![bin.clone()],
        ..Default::default()
    };
    let env = child_env(&policy, false);
    let get = |key: &str| {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.to_string_lossy().into_owned())
    };
    assert_eq!(get("HOME").as_deref(), Some(root.to_str().unwrap()));
    assert_eq!(get("CARGO_HOME").as_deref(), Some("/real/.cargo"));
    let path = get("PATH").unwrap();
    assert!(path.starts_with(bin.to_str().unwrap()), "{path}");
    assert!(
        path.len() > bin.to_str().unwrap().len(),
        "daemon PATH must follow"
    );
    // 中转线:HOME 不换,其余照给。
    let relay = child_env(&policy, true);
    assert!(relay.iter().all(|(name, _)| name != "HOME"));
    assert!(relay.iter().any(|(name, _)| name == "CARGO_HOME"));
}

#[tokio::test]
async fn no_policy_means_no_confinement() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("free.txt");
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-c")
        .arg(format!("echo hi > {}", target.display()));
    confine(&mut command);
    assert!(command.status().await.unwrap().success());
    assert!(target.is_file());
}

mod distribution_sandbox;
