use std::fs;
use std::path::Path;

use ::settings::Setting;
use lsp::supported_servers::LSPServerType;
use repo_metadata::repositories::DetectedRepositories;
use warpui::{App, SingletonEntity};

use super::{enabled_servers_for_path, relative_log_path, repo_root_for_path};
use crate::settings::CodeSettings;
use crate::test_util::settings::initialize_settings_for_tests;

#[test]
fn no_servers_enabled_by_default() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        app.read(|ctx| {
            assert!(enabled_servers_for_path(Path::new("/repo/main.py"), ctx).is_empty());
        });
    })
}

#[test]
fn enabled_servers_match_file_language() {
    App::test((), |mut app| async move {
        initialize_settings_for_tests(&mut app);
        CodeSettings::handle(&app).update(&mut app, |settings, ctx| {
            settings
                .enabled_lsp_servers
                .set_value(vec![LSPServerType::Pyright, LSPServerType::GoPls], ctx)
                .expect("Can enable servers");
        });

        app.read(|ctx| {
            assert_eq!(
                enabled_servers_for_path(Path::new("/repo/main.py"), ctx),
                vec![LSPServerType::Pyright]
            );
            assert_eq!(
                enabled_servers_for_path(Path::new("/repo/main.go"), ctx),
                vec![LSPServerType::GoPls]
            );
            assert!(enabled_servers_for_path(Path::new("/repo/index.ts"), ctx).is_empty());
            assert!(enabled_servers_for_path(Path::new("/repo/README.md"), ctx).is_empty());
        });
    })
}

#[test]
fn repo_root_falls_back_to_git_directory() {
    App::test((), |app| async move {
        app.add_singleton_model(|_| DetectedRepositories::default());

        let repo = tempfile::tempdir().expect("Can create temp dir");
        let repo_root = repo.path().canonicalize().expect("Can canonicalize");
        fs::create_dir(repo_root.join(".git")).expect("Can create .git");
        fs::create_dir_all(repo_root.join("src/pkg")).expect("Can create src");
        let file = repo_root.join("src/pkg/main.py");
        fs::write(&file, "").expect("Can write file");

        let outside = tempfile::tempdir().expect("Can create temp dir");
        let loose_file = outside.path().join("main.py");
        fs::write(&loose_file, "").expect("Can write file");

        app.read(|ctx| {
            assert_eq!(repo_root_for_path(&file, ctx), Some(repo_root.clone()));
            assert_eq!(repo_root_for_path(&loose_file, ctx), None);
        });
    })
}

#[test]
fn log_path_is_stable_per_server_and_root() {
    let a = relative_log_path(LSPServerType::Pyright, Path::new("/repo/a"));
    assert_eq!(
        a,
        relative_log_path(LSPServerType::Pyright, Path::new("/repo/a"))
    );
    assert_ne!(
        a,
        relative_log_path(LSPServerType::Pyright, Path::new("/repo/b"))
    );
    assert!(a.starts_with(LSPServerType::Pyright.binary_name()));
}
