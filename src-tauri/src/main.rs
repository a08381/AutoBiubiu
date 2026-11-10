// 调试版与发行版都使用 GUI 子系统；测试程序仍保留控制台输出。
#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

fn main() {
    autobiubiu_lib::run();
}
