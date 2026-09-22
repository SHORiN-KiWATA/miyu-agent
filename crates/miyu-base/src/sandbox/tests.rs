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
