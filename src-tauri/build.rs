fn main() {
    // 生产构建里前端页面由本机 HTTP 提供（见 src/webserve.rs），在 Tauri 眼里
    // 属于“远程 origin”；远程 origin 下应用命令必须逐条声明权限，否则 IPC 会
    // 被 ACL 拒绝（表现为 “get_status not allowed”）。这里把所有 #[tauri::command]
    // 列全，build 会为每条生成 allow-<命令名> 权限，供 capabilities 引用。
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&[
                "load_config",
                "default_config",
                "save_config",
                "save_window_size",
                "get_status",
                "start_dsh",
                "stop_dsh",
                "restart_dsh",
                "start_safe_mode",
                "exit_safe_mode",
                "submit_auth_url",
                "auth_url_consumed",
                "clear_logs",
                "open_workspace",
                "list_config_files",
                "save_dsh_config",
                "open_dsh_config",
                "get_package_info",
                "update_dsh",
                "get_launcher_version",
                "check_launcher_update",
                "list_plugins",
                "search_plugins",
                "install_plugin",
                "remove_plugin",
                "update_plugins",
                "check_plugin_updates",
                "fetch_market",
                "open_external",
                "new_launcher_window",
                "take_initial_tab",
                "show_tab_drag_preview",
                "move_tab_drag_preview",
                "hide_tab_drag_preview",
                "drop_tab",
            ]),
        ),
    )
    .expect("failed to run tauri-build");
}
