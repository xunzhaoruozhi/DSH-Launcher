// Prevents an extra console window behind the launcher on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 必须先于 run() 里的单实例插件：自愈重启的新进程要在这里结束卡死的
    // 旧进程，否则会被旧实例的单实例锁挡在外面。
    dsh_launcher_lib::consume_restart_marker();
    dsh_launcher_lib::run();
}
