#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod docker;
mod dto;
mod events;

use std::sync::Arc;

use tauri::Manager;

use crate::commands::{ipc, AppState};

/// The catalog lives in the OS's per-app data directory
/// (`~/Library/Application Support/com.mitko.storagelens` on macOS,
/// `%APPDATA%\com.mitko.storagelens` on Windows). Nothing leaves the machine.
const CATALOG_FILE: &str = "catalog.db";

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let state = AppState::open(data_dir.join(CATALOG_FILE))?;
            app.manage(Arc::new(state));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::list_scan_targets,
            ipc::start_scan,
            ipc::cancel_scan,
            ipc::scan_status,
            ipc::latest_scan,
            ipc::get_overview,
            ipc::get_categories,
            ipc::get_grouping,
            ipc::get_children,
            ipc::get_tree,
            ipc::reveal_entry,
            ipc::preview_entry,
            ipc::get_files,
            ipc::get_largest,
            ipc::search,
            ipc::get_ancestors,
            ipc::get_rules,
            ipc::preview_deletion,
            ipc::execute_deletion,
            ipc::cancel_deletion,
            ipc::undo_deletion,
            ipc::list_apps,
            ipc::force_quit_app,
            ipc::uninstall_app,
            ipc::uninstall_finished,
            ipc::app_leftovers,
            ipc::preview_leftover_removal,
            ipc::remove_registry_leftovers,
            ipc::restore_registry_backups,
            ipc::remove_system_leftovers,
            ipc::list_startup,
            ipc::set_startup,
            ipc::open_startup_settings,
            ipc::preview_app_removal,
            ipc::find_duplicates,
            ipc::cancel_duplicates,
            ipc::preview_duplicate_removal,
            ipc::docker_contents,
            ipc::remove_docker,
            ipc::check_full_disk_access,
            ipc::open_full_disk_access_settings,
            ipc::request_finder_access,
            ipc::check_app_management,
            ipc::open_privacy_pane,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start StorageLens");
}
