use std::{fs, path::Path};

use command::r#async::Command;
use command::Stdio;
use tempfile::TempDir;

use super::{
    commit_file_diff_paths, detect_current_branch, detect_current_branch_display,
    get_commit_file_diff, get_commit_files, get_commit_history, parse_commit_history,
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

    let merge_hash = git(&repo, &["rev-parse", "HEAD"]);
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

    let root_hash = git(&repo, &["rev-parse", "HEAD"]);
    let files = get_commit_files(&repo, &root_hash).await.unwrap();
    let diff = get_commit_file_diff(&repo, &root_hash, "root.txt")
        .await
        .unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "root.txt");
    assert!(diff.patch.contains("+root content"));
}
