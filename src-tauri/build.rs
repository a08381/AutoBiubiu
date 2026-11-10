fn main() {
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
    let mut attributes = tauri_build::Attributes::new();
    // 发行版在启动时统一请求管理员权限，新启动的 biubiu 子进程继承权限。
    // 开发与单元测试保留普通权限，避免 Cargo 启动测试程序时遇到错误 740。
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        attributes = attributes.windows_attributes(
            tauri_build::WindowsAttributes::new()
                .app_manifest(include_str!("windows-app-manifest.xml")),
        );
    }
    tauri_build::try_build(attributes).expect("Tauri 构建配置失败");
}
