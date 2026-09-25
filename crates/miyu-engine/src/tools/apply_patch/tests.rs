use super::*;

/// kb 补丁三操作全链路:写入/更新/删除都必须落到 KnowledgeBase
/// (kb_meta.db 有行、文件在 kb 根下),而不是裸 fs 写。
#[tokio::test]
async fn kb_patch_routes_writes_through_the_knowledge_base() {
    let temp = tempfile::tempdir().unwrap();
    let paths = crate::tools::tests::test_paths(temp.path());
    let config = miyu_base::config::AppConfig::default();
    let kb =
        crate::tools::knowledge_base::KnowledgeBase::new(config.clone(), paths.clone()).unwrap();
    kb.init().unwrap();

    let add = "*** Begin Patch\n*** Add File: notes/demo.md\n+# demo\n+hello kb\n*** End Patch";
    let output = apply_kb_patch(
        json!({ "patchText": add }),
        ToolProgress::default(),
        &config,
        &paths,
    )
    .unwrap();
    assert!(output.contains("kb:notes/demo.md"), "{output}");
    let stored = kb.safe_file_path("notes/demo.md").unwrap();
    assert_eq!(
        std::fs::read_to_string(&stored).unwrap(),
        "# demo\nhello kb\n"
    );

    let update = "*** Begin Patch\n*** Update File: notes/demo.md\n@@ # demo\n-hello kb\n+hello again\n*** End Patch";
    apply_kb_patch(
        json!({ "patchText": update }),
        ToolProgress::default(),
        &config,
        &paths,
    )
    .unwrap();
    assert!(std::fs::read_to_string(&stored)
        .unwrap()
        .contains("hello again"));

    let delete = "*** Begin Patch\n*** Delete File: notes/demo.md\n*** End Patch";
    apply_kb_patch(
        json!({ "patchText": delete }),
        ToolProgress::default(),
        &config,
        &paths,
    )
    .unwrap();
    assert!(!stored.exists());
}

/// edit 收到带域前缀的补丁必须指路而不是走错域。
#[test]
fn edit_rejects_prefixed_patches_with_a_pointer() {
    let patch = "*** Begin Patch\n*** Add File: kb:notes/x.md\n+x\n*** End Patch";
    let error = edit_filesystem(json!({ "patchText": patch }), ToolProgress::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("`kb` tool"), "{error}");
    let patch = "*** Begin Patch\n*** Add File: artifact:r.md\n+x\n*** End Patch";
    let error = edit_filesystem(json!({ "patchText": patch }), ToolProgress::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("`artifact` tool"), "{error}");
}

#[test]
fn parses_add_update_delete_patch() {
    let patch = "*** Begin Patch\n*** Add File: a.txt\n+hello\n*** Update File: b.txt\n@@ marker\n-old\n+new\n*** Delete File: c.txt\n*** End Patch";
    let operations = parse_patch_with(patch, &path_arg).unwrap();
    assert_eq!(operations.len(), 3);
}

#[test]
fn update_hunk_replaces_exact_text() {
    let path = PathBuf::from("demo.txt");
    let hunk = Hunk {
        context: None,
        end_of_file: false,
        lines: vec![
            HunkLine::Context("one".to_string()),
            HunkLine::Delete("two".to_string()),
            HunkLine::Insert("TWO".to_string()),
            HunkLine::Context("three".to_string()),
        ],
    };
    let (result, _) = apply_hunk(&path, "one\ntwo\nthree\n", &hunk, 0).unwrap();
    assert_eq!(result, "one\nTWO\nthree\n");
}

#[test]
fn update_hunk_fails_when_stale() {
    let path = PathBuf::from("demo.txt");
    let hunk = Hunk {
        context: None,
        end_of_file: false,
        lines: vec![
            HunkLine::Delete("missing".to_string()),
            HunkLine::Insert("new".to_string()),
        ],
    };
    assert!(apply_hunk(&path, "current\n", &hunk, 0).is_err());
}

#[test]
fn parses_fenced_patch_with_no_space_after_header_colon() {
    let patch = "```\n*** Begin Patch\n*** Add File:a.txt\n+hello\n*** End Patch\n```";
    let operations = parse_patch_with(patch, &path_arg).unwrap();
    assert_eq!(operations.len(), 1);
}

#[test]
fn insertion_hunk_uses_context_header() {
    let path = PathBuf::from("demo.txt");
    let hunk = Hunk {
        context: Some("one".to_string()),
        end_of_file: false,
        lines: vec![HunkLine::Insert("inserted".to_string())],
    };
    let (result, _) = apply_hunk(&path, "one\ntwo\n", &hunk, 0).unwrap();
    assert_eq!(result, "one\ninserted\ntwo\n");
}

#[test]
fn apply_patch_adds_updates_and_deletes_files() {
    let temp = tempfile::tempdir().unwrap();
    let keep = temp.path().join("keep.txt");
    let remove = temp.path().join("remove.txt");
    std::fs::write(&keep, "one\ntwo\nthree\n").unwrap();
    std::fs::write(&remove, "delete me\n").unwrap();

    let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+new file\n*** Update File: {}\n@@ patch\n one\n-two\n+TWO\n three\n*** Delete File: {}\n*** End Patch",
            temp.path().join("new.txt").display(),
            keep.display(),
            remove.display()
        );
    let result = apply_patch(json!({ "patchText": patch }), ToolProgress::default()).unwrap();
    let data: Value = serde_json::from_str(&result).unwrap();

    assert_eq!(data["ok"], true);
    assert_eq!(data["files_changed"], 3);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("new.txt")).unwrap(),
        "new file\n"
    );
    assert_eq!(std::fs::read_to_string(&keep).unwrap(), "one\nTWO\nthree\n");
    assert!(!remove.exists());
}

#[test]
fn apply_patch_repeated_update_sections_use_staged_content() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("repeated.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n@@ first\n-one\n+ONE\n*** Update File: {}\n@@ second\n ONE\n-two\n+TWO\n three\n*** End Patch",
            file.display(),
            file.display()
        );
    let result = apply_patch(json!({ "patchText": patch }), ToolProgress::default()).unwrap();
    let data: Value = serde_json::from_str(&result).unwrap();

    assert_eq!(data["ok"], true);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "ONE\nTWO\nthree\n");
}

#[test]
fn apply_patch_rejects_move_to_until_supported() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    let target = temp.path().join("target.txt");
    std::fs::write(&source, "old\n").unwrap();
    let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@ patch\n-old\n+new\n*** End Patch",
            source.display(),
            target.display()
        );

    assert!(apply_patch(json!({ "patchText": patch }), ToolProgress::default()).is_err());
    assert!(source.exists());
    assert!(!target.exists());
}

#[test]
fn artifact_patch_adds_and_updates_managed_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("artifacts");
    let session_dir = root.join("sess_test");
    std::fs::create_dir_all(&session_dir).unwrap();
    let report = session_dir.join("report.md");
    std::fs::write(&report, "# Report\n\nOld text.\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: report.md\n@@ report\n # Report\n \n-Old text.\n+Updated text.\n*** Add File: notes.txt\n+Follow up.\n*** End Patch";
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let output = apply_artifact_patch(
        json!({"patchText": patch}),
        ToolProgress::new(sender),
        &root,
        "sess_test",
    )
    .unwrap();
    let payload: Value = serde_json::from_str(&output).unwrap();

    assert_eq!(payload["operation"], "apply_artifact_patch");
    assert_eq!(payload["files_changed"], 2);
    assert_eq!(payload["files"][0]["path"], "report.md");
    assert!(!output.contains(temp.path().to_string_lossy().as_ref()));
    assert_eq!(
        std::fs::read_to_string(&report).unwrap(),
        "# Report\n\nUpdated text.\n"
    );
    let notes = session_dir.join("notes.txt");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "Follow up.\n");
    for path in [&report, &notes] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let artifacts = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|event| match event {
            super::super::ToolProgressEvent::Artifact { path, .. } => Some(path),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(artifacts, [report, notes]);
}

#[test]
fn artifact_patch_rejects_unsafe_paths_and_symlinks_but_allows_delete() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("artifacts");
    let session_dir = root.join("sess_test");
    std::fs::create_dir_all(&session_dir).unwrap();
    let report = session_dir.join("report.md");
    std::fs::write(&report, "original\n").unwrap();
    let outside = temp.path().join("outside.md");
    std::fs::write(&outside, "outside\n").unwrap();
    symlink(&outside, session_dir.join("link.md")).unwrap();

    for patch in [
        "*** Begin Patch\n*** Add File: ../escape.md\n+bad\n*** End Patch",
        "*** Begin Patch\n*** Add File: nested/file.md\n+bad\n*** End Patch",
        "*** Begin Patch\n*** Update File: link.md\n@@ patch\n-outside\n+changed\n*** End Patch",
        "*** Begin Patch\n*** Delete File: link.md\n*** End Patch",
    ] {
        assert!(apply_artifact_patch(
            json!({"patchText": patch}),
            ToolProgress::default(),
            &root,
            "sess_test",
        )
        .is_err());
    }
    assert_eq!(std::fs::read_to_string(&report).unwrap(), "original\n");
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside\n");
    assert!(!temp.path().join("escape.md").exists());

    // 验收四轮:Artifact 与本地补丁同语义,普通文件的 Delete File 放行。
    let deleted = apply_artifact_patch(
        json!({"patchText": "*** Begin Patch\n*** Delete File: report.md\n*** End Patch"}),
        ToolProgress::default(),
        &root,
        "sess_test",
    );
    assert!(deleted.is_ok(), "{deleted:?}");
    assert!(!report.exists());
}

fn update_patch(path: &std::path::Path, hunks: &str) -> String {
    format!(
        "*** Begin Patch\n*** Update File: {}\n{hunks}\n*** End Patch",
        path.display()
    )
}

/// 09-24 B2：改完的文件要保住原来的权限位。tempfile 在 Unix 上按 0600 建临时文件，
/// rename 之后目标就成了 0600——改一次脚本就丢执行位。
#[cfg(unix)]
#[test]
fn edit_keeps_the_files_permission_bits() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let script = temp.path().join("run.sh");
    std::fs::write(&script, "#!/bin/sh\necho old\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    edit_filesystem(
        json!({ "patchText": update_patch(&script, "@@\n-echo old\n+echo new") }),
        ToolProgress::default(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(&script).unwrap(),
        "#!/bin/sh\necho new\n"
    );
    assert_eq!(
        std::fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

/// 09-24 B2：改软链时写到它指向的文件，软链本身留着（原来 rename 把软链换成了普通文件）。
#[cfg(unix)]
#[test]
fn edit_writes_through_a_symlink() {
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let real = temp.path().join("real.conf");
    let link = temp.path().join("link.conf");
    std::fs::write(&real, "alpha\n").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    edit_filesystem(
        json!({ "patchText": update_patch(&link, "@@\n-alpha\n+beta") }),
        ToolProgress::default(),
    )
    .unwrap();

    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "beta\n");
}

/// 09-24 B10：精确匹配唯一时，后面一处只差缩进的同文不算歧义（原来四档一起数，误报）。
#[test]
fn an_exact_match_is_not_ambiguous_with_a_differently_indented_copy() {
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let file = temp.path().join("lib.rs");
    std::fs::write(
        &file,
        "fn a() {\n    return Ok(());\n}\nfn b() {\n        return Ok(());\n}\n",
    )
    .unwrap();

    edit_filesystem(
        json!({ "patchText": update_patch(&file, "@@\n-    return Ok(());\n+    return Err(());") }),
        ToolProgress::default(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn a() {\n    return Err(());\n}\nfn b() {\n        return Ok(());\n}\n"
    );
}

/// 09-24 B10：从头找有两处时，取上一块改完之后唯一的那一处——补丁里的块按文件
/// 顺序写。匹配唯一时照旧从头找，乱序写的补丁不受影响。
#[test]
fn an_ambiguous_hunk_resolves_to_the_copy_after_the_previous_hunk() {
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let file = temp.path().join("notes.txt");
    std::fs::write(&file, "top\nsame\nmiddle\nsame\nbottom\n").unwrap();

    edit_filesystem(
        json!({ "patchText": update_patch(&file, "@@\n-middle\n+MIDDLE\n@@\n-same\n+SAME") }),
        ToolProgress::default(),
    )
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "top\nsame\nMIDDLE\nSAME\nbottom\n"
    );
}

/// 09-24 B11：补丁里的 Delete File 进回收站，不是永久删除。
#[test]
fn deleting_a_file_moves_it_to_the_trash() {
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let file = temp.path().join("old.txt");
    std::fs::write(&file, "bye\n").unwrap();

    edit_filesystem(
        json!({ "patchText": format!("*** Begin Patch\n*** Delete File: {}\n*** End Patch", file.display()) }),
        ToolProgress::default(),
    )
    .unwrap();

    assert!(!file.exists());
    assert!(test_support::trashed().contains(&file));
}

/// 09-24 B10：多文件补丁写到一半失败时，报错说清哪些文件已经写进去了。
#[test]
fn a_failed_multi_file_patch_names_what_was_already_written() {
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let first = temp.path().join("first.txt");
    let blocked_dir = temp.path().join("blocked");
    std::fs::write(&first, "one\n").unwrap();
    // 第二个文件的父路径是个普通文件：预检过得去，真正写的时候建目录失败。
    std::fs::write(&blocked_dir, "not a directory").unwrap();
    let second = blocked_dir.join("second.txt");
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-one\n+ONE\n*** Add File: {}\n+two\n*** End Patch",
        first.display(),
        second.display()
    );

    let error = edit_filesystem(json!({ "patchText": patch }), ToolProgress::default())
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("Already applied before this failure"),
        "{error}"
    );
    assert!(error.contains("first.txt"), "{error}");
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "ONE\n");
}
