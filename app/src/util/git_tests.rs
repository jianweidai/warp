use std::{fs, path::Path};

use command::r#async::Command;
use command::Stdio;
use tempfile::TempDir;

use super::{
    commit_file_diff_paths, detect_current_branch, detect_current_branch_display,
    get_commit_file_diff, get_commit_files, get_commit_history, get_working_tree_changes,
    get_working_tree_file_diff, parse_commit_history, parse_working_tree_changes,
    GitWorkingTreeArea, GitWorkingTreeChangeStatus,
};

/// Helper: run a git command inside the given repo directory.
async fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("failed to run git");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Creates a temp git repo with one commit and returns `(dir_handle, repo_path)`.
async fn init_repo() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().to_path_buf();

    git(&path, &["init", "-b", "main"]).await;
    git(&path, &["config", "user.email", "test@test.com"]).await;
    git(&path, &["config", "user.name", "Test"]).await;
    git(&path, &["commit", "--allow-empty", "-m", "initial"]).await;

    (dir, path)
}

#[tokio::test]
async fn on_normal_branch_returns_branch_name() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["checkout", "-b", "feature-xyz"]).await;

    assert_eq!(detect_current_branch(&repo).await.unwrap(), "feature-xyz");
    assert_eq!(
        detect_current_branch_display(&repo).await.unwrap(),
        "feature-xyz"
    );
}

#[tokio::test]
async fn detached_head_raw_returns_head() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["checkout", "--detach", "HEAD"]).await;

    assert_eq!(detect_current_branch(&repo).await.unwrap(), "HEAD");
}

#[tokio::test]
async fn detached_head_display_returns_short_sha() {
    let (_dir, repo) = init_repo().await;
    let full_sha = git(&repo, &["rev-parse", "HEAD"]).await;
    git(&repo, &["checkout", "--detach", "HEAD"]).await;

    let result = detect_current_branch_display(&repo).await.unwrap();

    assert_ne!(
        result, "HEAD",
        "display variant should not return literal HEAD"
    );
    assert!(
        full_sha.starts_with(&result),
        "expected {full_sha} to start with {result}"
    );
}

#[tokio::test]
async fn detached_tag_display_returns_short_sha() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["tag", "v1.0"]).await;
    git(&repo, &["checkout", "v1.0"]).await;

    let full_sha = git(&repo, &["rev-parse", "HEAD"]).await;
    let result = detect_current_branch_display(&repo).await.unwrap();

    assert_ne!(result, "HEAD");
    assert!(
        full_sha.starts_with(&result),
        "expected {full_sha} to start with {result}"
    );
}

#[test]
fn parses_commit_history_records() {
    let output =
        "abc\u{0}abc1234\u{0}Test User\u{0}2026-08-25T12:34:56+08:00\u{0}Add history\u{1}\n";
    let commits = parse_commit_history(output);

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].hash, "abc");
    assert_eq!(commits[0].short_hash, "abc1234");
    assert_eq!(commits[0].author, "Test User");
    assert_eq!(commits[0].subject, "Add history");
}

#[test]
fn parses_staged_unstaged_untracked_and_renamed_changes() {
    let output = concat!(
        "1 M. N... 100644 100644 100644 a b staged.txt\0",
        "1 .M N... 100644 100644 100644 a b unstaged.txt\0",
        "1 MM N... 100644 100644 100644 a b both.txt\0",
        "2 R. N... 100644 100644 100644 a b R100 new name.txt\0",
        "old name.txt\0",
        "u UU N... 100644 100644 100644 100644 a b c conflict.txt\0",
        "? untracked.txt\0",
    );

    let changes = parse_working_tree_changes(output);

    assert_eq!(changes.staged.len(), 3);
    assert_eq!(changes.unstaged.len(), 4);
    assert_eq!(changes.staged[0].path, "both.txt");
    assert_eq!(changes.unstaged[0].path, "both.txt");
    assert_eq!(
        changes.staged[1].status,
        GitWorkingTreeChangeStatus::Renamed {
            old_path: "old name.txt".to_string(),
        }
    );
    assert_eq!(
        changes.unstaged[1].status,
        GitWorkingTreeChangeStatus::Conflicted
    );
    assert_eq!(
        changes.unstaged[3].status,
        GitWorkingTreeChangeStatus::Untracked
    );
}

#[tokio::test]
async fn working_tree_changes_and_diffs_keep_staged_and_unstaged_content_separate() {
    let (_dir, repo) = init_repo().await;
    fs::write(repo.join("tracked.txt"), "original\n").expect("failed to write tracked file");
    git(&repo, &["add", "tracked.txt"]).await;
    git(&repo, &["commit", "-m", "add tracked file"]).await;

    fs::write(repo.join("staged.txt"), "staged content\n").expect("failed to write staged file");
    git(&repo, &["add", "staged.txt"]).await;
    fs::write(repo.join("tracked.txt"), "working content\n")
        .expect("failed to update tracked file");
    fs::write(repo.join("untracked.txt"), "untracked content\n")
        .expect("failed to write untracked file");

    let changes = get_working_tree_changes(&repo).await.unwrap();
    let staged = changes
        .staged
        .iter()
        .find(|change| change.path == "staged.txt")
        .unwrap();
    let unstaged = changes
        .unstaged
        .iter()
        .find(|change| change.path == "tracked.txt")
        .unwrap();

    let staged_diff = get_working_tree_file_diff(&repo, GitWorkingTreeArea::Staged, staged)
        .await
        .unwrap();
    let unstaged_diff = get_working_tree_file_diff(&repo, GitWorkingTreeArea::Unstaged, unstaged)
        .await
        .unwrap();

    assert!(staged_diff.patch.contains("+staged content"));
    assert!(!staged_diff.patch.contains("working content"));
    assert!(unstaged_diff.patch.contains("+working content"));
    assert!(!unstaged_diff.patch.contains("staged content"));
}

#[tokio::test]
async fn commit_history_supports_first_page_and_pagination() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["commit", "--allow-empty", "-m", "second"]).await;

    let first_page = get_commit_history(&repo, 0, 1).await.unwrap();
    assert_eq!(first_page.commits.len(), 1);
    assert_eq!(first_page.commits[0].subject, "second");
    assert!(first_page.has_more);

    let second_page = get_commit_history(&repo, 1, 1).await.unwrap();
    assert_eq!(second_page.commits.len(), 1);
    assert_eq!(second_page.commits[0].subject, "initial");
    assert!(!second_page.has_more);
}

#[tokio::test]
async fn commit_file_diff_uses_committed_content_not_worktree_content() {
    let (_dir, repo) = init_repo().await;
    let file_path = repo.join("README.md");
    fs::write(&file_path, "committed content\n").expect("failed to write committed file");
    git(&repo, &["add", "README.md"]).await;
    git(&repo, &["commit", "-m", "add readme"]).await;
    let commit_hash = git(&repo, &["rev-parse", "HEAD"]).await;

    fs::write(&file_path, "uncommitted content\n").expect("failed to update worktree file");

    let diff = get_commit_file_diff(&repo, &commit_hash, "README.md")
        .await
        .unwrap();

    assert!(!diff.is_binary);
    assert!(diff.patch.contains("+committed content"));
    assert!(!diff.patch.contains("uncommitted content"));
}

#[test]
fn rename_display_paths_expand_to_both_pathspecs() {
    assert_eq!(
        commit_file_diff_paths("src/{old_name.rs => new_name.rs}"),
        vec!["src/old_name.rs".to_owned(), "src/new_name.rs".to_owned()]
    );
    assert_eq!(
        commit_file_diff_paths("old_name.rs => new_name.rs"),
        vec!["old_name.rs".to_owned(), "new_name.rs".to_owned()]
    );
}

#[tokio::test]
async fn commit_files_use_first_parent_for_merge_commits() {
    let (_dir, repo) = init_repo().await;
    git(&repo, &["checkout", "-b", "feature"]).await;
    fs::write(repo.join("feature.txt"), "feature\n").expect("failed to write feature file");
    git(&repo, &["add", "feature.txt"]).await;
    git(&repo, &["commit", "-m", "feature change"]).await;
    git(&repo, &["checkout", "main"]).await;
    fs::write(repo.join("main.txt"), "main\n").expect("failed to write main file");
    git(&repo, &["add", "main.txt"]).await;
    git(&repo, &["commit", "-m", "main change"]).await;
    git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "merge feature"],
    )
    .await;

    let merge_hash = git(&repo, &["rev-parse", "HEAD"]).await;
    let files = get_commit_files(&repo, &merge_hash).await.unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "feature.txt");
}

#[tokio::test]
async fn commit_files_and_diff_support_root_commits() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let repo = dir.path().to_path_buf();
    git(&repo, &["init", "-b", "main"]).await;
    git(&repo, &["config", "user.email", "test@test.com"]).await;
    git(&repo, &["config", "user.name", "Test"]).await;
    fs::write(repo.join("root.txt"), "root content\n").expect("failed to write root file");
    git(&repo, &["add", "root.txt"]).await;
    git(&repo, &["commit", "-m", "root commit"]).await;

    let root_hash = git(&repo, &["rev-parse", "HEAD"]).await;
    let files = get_commit_files(&repo, &root_hash).await.unwrap();
    let diff = get_commit_file_diff(&repo, &root_hash, "root.txt")
        .await
        .unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "root.txt");
    assert!(diff.patch.contains("+root content"));
}

#[test]
fn changed_file_count_counts_unique_paths() {
    let porcelain = concat!(
        "1 MM N... 100644 100644 100644 0 0 src/main.rs\0",
        "? untracked.txt\0",
        "1 A. N... 100644 100644 100644 0 0 added.rs\0",
    );
    let changes = parse_working_tree_changes(porcelain);
    assert_eq!(changes.staged.len(), 2, "MM staged + A. staged");
    assert_eq!(changes.unstaged.len(), 2, "MM unstaged + untracked");
    assert_eq!(changes.changed_file_count(), 3);
}

#[test]
fn changed_file_count_is_zero_for_empty_tree() {
    assert_eq!(parse_working_tree_changes("").changed_file_count(), 0);
}

