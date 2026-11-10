use serde::Serialize;
use std::path::{Path, PathBuf};

const HELPER_FLAG: &str = "--autobiubiu-client-helper";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub executable_path: String,
    pub source: String,
}

pub fn validate_executable(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || !path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("biubiu.exe"))
        || !path.is_file()
    {
        return Err("请选择实际存在的 biubiu 安装目录或 biubiu.exe 完整路径".into());
    }
    Ok(path.to_owned())
}

fn icon_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    let text = if let Some(rest) = value.strip_prefix('"') {
        &rest[..rest.find('"')?]
    } else {
        let end = value.to_ascii_lowercase().rfind(".exe")? + 4;
        &value[..end]
    };
    Some(PathBuf::from(text))
}

// ShellExecuteEx 接受一条参数字符串，按 Windows CommandLineToArgvW 规则引用每个参数。
fn quote_argument(value: &str) -> String {
    let mut output = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            output.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            output.push_str(&"\\".repeat(slashes));
        }
        output.push(ch);
        slashes = 0;
    }
    output.push_str(&"\\".repeat(slashes * 2));
    output.push('"');
    output
}

// VS Code 等宿主可能设置这些变量，传给 Electron 客户端会改变运行模式。
// 只清理新客户端的环境，不修改 AutoBiubiu、自身辅助进程或系统环境。
const HOST_RUNTIME_VARIABLES: [&str; 4] = [
    "ELECTRON_RUN_AS_NODE",
    "ELECTRON_NO_ASAR",
    "NODE_OPTIONS",
    "NODE_PATH",
];

fn client_command(executable: &Path, port: u16) -> Result<std::process::Command, String> {
    let mut command = std::process::Command::new(executable);
    command
        .args([
            format!("--remote-debugging-port={port}"),
            "--remote-debugging-address=127.0.0.1".into(),
        ])
        .current_dir(executable.parent().ok_or("安装目录无效")?)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for variable in HOST_RUNTIME_VARIABLES {
        command.env_remove(variable);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    Ok(command)
}

fn check_initial_exit(mut child: std::process::Child) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("读取客户端启动状态失败：{e}"))?
        {
            // 客户端升级/转交主实例可能正常退出，仍由后续 CDP 探测确认就绪。
            return if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "biubiu 启动后立即退出（退出码 {}），请检查客户端安装或尝试手动打开客户端",
                    status
                        .code()
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "未知".into())
                ))
            };
        }
        if std::time::Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        collections::HashSet,
        ffi::OsStr,
        mem::size_of,
        os::windows::ffi::OsStrExt,
        ptr::{null, null_mut},
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, ERROR_CANCELLED, ERROR_NO_MORE_ITEMS, HANDLE,
            INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Environment::ExpandEnvironmentStringsW,
            Registry::*,
            Threading::{
                GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
                QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
            },
        },
        UI::{
            Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
            WindowsAndMessaging::{
                MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_SETFOREGROUND,
                MB_YESNO, SW_HIDE,
            },
        },
    };

    fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
        value.as_ref().encode_wide().chain(Some(0)).collect()
    }
    fn text(value: &[u16]) -> String {
        String::from_utf16_lossy(
            &value[..value.iter().position(|v| *v == 0).unwrap_or(value.len())],
        )
    }
    fn last_error(context: &str) -> String {
        format!("{context}：{}", std::io::Error::last_os_error())
    }
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    fn key(root: HKEY, name: &str, view: u32) -> Option<Key> {
        let mut handle = null_mut();
        if unsafe { RegOpenKeyExW(root, wide(name).as_ptr(), 0, KEY_READ | view, &mut handle) } == 0
        {
            Some(Key(handle))
        } else {
            None
        }
    }
    fn value(key: &Key, name: &str) -> Option<String> {
        let name = wide(name);
        let mut kind = 0;
        let mut bytes = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                null(),
                &mut kind,
                null_mut(),
                &mut bytes,
            )
        };
        if status != 0 || !matches!(kind, REG_SZ | REG_EXPAND_SZ) || bytes > 65_536 {
            return None;
        }
        let mut buffer = vec![0u16; bytes as usize / 2 + 1];
        if unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                null(),
                &mut kind,
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        } != 0
        {
            return None;
        }
        let raw = text(&buffer);
        if kind != REG_EXPAND_SZ {
            return Some(raw);
        }
        let raw = wide(&raw);
        let len = unsafe { ExpandEnvironmentStringsW(raw.as_ptr(), null_mut(), 0) };
        if len == 0 || len > 32768 {
            return None;
        }
        let mut expanded = vec![0u16; len as usize];
        let copied = unsafe { ExpandEnvironmentStringsW(raw.as_ptr(), expanded.as_mut_ptr(), len) };
        (copied > 0 && copied <= len).then(|| text(&expanded))
    }

    pub fn discover() -> Result<Vec<Installation>, String> {
        let mut candidates: Vec<(PathBuf, String)> = Vec::new();
        for (root, label) in [
            (HKEY_CURRENT_USER, "当前用户注册表"),
            (HKEY_LOCAL_MACHINE, "系统安装列表"),
        ] {
            for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                if let Some(app) = key(
                    root,
                    r"Software\Microsoft\Windows\CurrentVersion\App Paths\biubiu.exe",
                    view,
                ) {
                    if let Some(path) = value(&app, "") {
                        if let Some(path) = icon_path(&path) {
                            candidates.push((path, label.into()));
                        }
                    }
                }
                if let Some(uninstall) = key(
                    root,
                    r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
                    view,
                ) {
                    for index in 0..10_000 {
                        let mut name = [0u16; 256];
                        let mut len = name.len() as u32;
                        let status = unsafe {
                            RegEnumKeyExW(
                                uninstall.0,
                                index,
                                name.as_mut_ptr(),
                                &mut len,
                                null(),
                                null_mut(),
                                null_mut(),
                                null_mut(),
                            )
                        };
                        if status == ERROR_NO_MORE_ITEMS {
                            break;
                        }
                        if status != 0 {
                            continue;
                        }
                        let Some(app) = key(uninstall.0, &text(&name[..len as usize]), view) else {
                            continue;
                        };
                        let display = value(&app, "DisplayName").unwrap_or_default();
                        if !display.to_ascii_lowercase().contains("biubiu") {
                            continue;
                        }
                        if let Some(location) = value(&app, "InstallLocation") {
                            candidates.push((
                                PathBuf::from(location.trim().trim_matches('"')).join("biubiu.exe"),
                                label.into(),
                            ));
                        }
                        if let Some(icon) = value(&app, "DisplayIcon").and_then(|v| icon_path(&v)) {
                            candidates.push((icon, label.into()));
                        }
                    }
                }
            }
        }
        for env in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(env) {
                for relative in [
                    r"biubiu\biubiu.exe",
                    r"Programs\biubiu\biubiu.exe",
                    r"biubiu加速器\biubiu.exe",
                ] {
                    candidates.push((PathBuf::from(&base).join(relative), "常见安装目录".into()));
                }
            }
        }
        let mut seen = HashSet::new();
        Ok(candidates
            .into_iter()
            .filter_map(|(path, source)| {
                let path = validate_executable(&path).ok()?;
                let identity = std::fs::canonicalize(&path)
                    .ok()?
                    .to_string_lossy()
                    .to_lowercase();
                seen.insert(identity).then(|| Installation {
                    executable_path: path.to_string_lossy().into_owned(),
                    source,
                })
            })
            .collect())
    }

    fn elevated() -> Result<bool, String> {
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(last_error("读取管理员状态失败"));
        }
        let token = Handle(token);
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0;
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
        } == 0
        {
            return Err(last_error("读取管理员状态失败"));
        }
        Ok(elevation.TokenIsElevated != 0)
    }
    fn image(handle: HANDLE) -> Result<PathBuf, String> {
        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        if unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len) } == 0 {
            return Err(last_error("读取客户端进程路径失败"));
        }
        Ok(PathBuf::from(String::from_utf16_lossy(
            &buffer[..len as usize],
        )))
    }
    fn same_image(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => {
                a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
            }
            _ => false,
        }
    }
    fn running(executable: &Path) -> Result<Vec<Handle>, String> {
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last_error("读取进程列表失败"));
        }
        let snapshot = Handle(raw);
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut result = Vec::new();
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while more {
            if text(&entry.szExeFile).eq_ignore_ascii_case("biubiu.exe") {
                let query = unsafe {
                    OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID)
                };
                if query.is_null() {
                    if crate::lease::process_alive(entry.th32ProcessID) {
                        return Err(last_error(
                            "无法确认现有 biubiu 进程路径，请手动退出客户端后重试",
                        ));
                    }
                } else {
                    let query = Handle(query);
                    if same_image(&image(query.0)?, executable) {
                        let process = unsafe {
                            OpenProcess(
                                PROCESS_QUERY_LIMITED_INFORMATION
                                    | PROCESS_TERMINATE
                                    | PROCESS_SYNCHRONIZE,
                                0,
                                entry.th32ProcessID,
                            )
                        };
                        if process.is_null() {
                            return Err(last_error("打开所选客户端进程失败"));
                        }
                        let process = Handle(process);
                        // 再次核对新句柄的路径，避免枚举后 PID 被复用。
                        if same_image(&image(process.0)?, executable) {
                            result.push(process);
                        }
                    }
                }
            }
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
        Ok(result)
    }

    fn start_elevated(executable: &Path, port: u16) -> Result<(), String> {
        let processes = running(executable)?;
        if !processes.is_empty() {
            let message = wide(format!(
                "需要重启以下客户端以开启本机调试端口 {port}：\n\n{}\n\n当前加速会中断。是否继续？",
                executable.display()
            ));
            let answer = unsafe {
                MessageBoxW(
                    null_mut(),
                    message.as_ptr(),
                    wide("AutoBiubiu · 确认重启客户端").as_ptr(),
                    MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING | MB_SETFOREGROUND,
                )
            };
            if answer != IDYES {
                return Err("已取消客户端重启".into());
            }
            for process in processes {
                if unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0 {
                    continue;
                }
                if unsafe { TerminateProcess(process.0, 0) } == 0 {
                    return Err(last_error("停止所选客户端失败"));
                }
                if unsafe { WaitForSingleObject(process.0, 5000) } != WAIT_OBJECT_0 {
                    return Err("等待客户端退出超时，请手动退出后重试".into());
                }
            }
            std::thread::sleep(Duration::from_millis(700));
        }
        let child = client_command(executable, port)?
            .spawn()
            .map_err(|e| format!("启动 biubiu 失败：{e}"))?;
        check_initial_exit(child)
    }

    pub fn launch(executable: &Path, port: u16) -> Result<(), String> {
        if elevated()? {
            return start_elevated(executable, port);
        }
        let own = std::env::current_exe().map_err(|e| format!("读取应用路径失败：{e}"))?;
        let parameters = wide(format!(
            "{HELPER_FLAG} {} {port}",
            quote_argument(&executable.to_string_lossy())
        ));
        let file = wide(own.as_os_str());
        let verb = wide("runas");
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.nShow = SW_HIDE;
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            if unsafe { GetLastError() } == ERROR_CANCELLED {
                return Err("已取消 Windows 管理员确认".into());
            }
            return Err(last_error("启动管理员连接进程失败"));
        }
        if info.hProcess.is_null() {
            return Err("未取得管理员连接进程句柄".into());
        }
        let helper = Handle(info.hProcess);
        let wait = unsafe { WaitForSingleObject(helper.0, 120_000) };
        if wait == WAIT_TIMEOUT {
            unsafe {
                TerminateProcess(helper.0, 1);
            }
            return Err("管理员连接确认超时，请重试".into());
        }
        if wait != WAIT_OBJECT_0 {
            return Err(last_error("等待管理员连接进程失败"));
        }
        let mut code = 1;
        if unsafe { GetExitCodeProcess(helper.0, &mut code) } == 0 {
            return Err(last_error("读取连接进程结果失败"));
        }
        if code != 0 {
            return Err("客户端连接未完成或已取消，请查看管理员确认窗口中的提示".into());
        }
        Ok(())
    }

    pub fn helper(executable: &Path, port: u16) -> i32 {
        let result = validate_executable(executable).and_then(|exe| {
            if port == 0 {
                return Err("调试端口必须是 1–65535".into());
            }
            if !elevated()? {
                return Err("连接进程未取得管理员权限".into());
            }
            start_elevated(&exe, port)
        });
        if let Err(error) = result {
            unsafe {
                MessageBoxW(
                    null_mut(),
                    wide(&error).as_ptr(),
                    wide("AutoBiubiu · 连接客户端").as_ptr(),
                    MB_ICONERROR | MB_SETFOREGROUND,
                );
            }
            1
        } else {
            0
        }
    }
}

pub fn discover_installations() -> Result<Vec<Installation>, String> {
    #[cfg(windows)]
    {
        windows::discover()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}
pub fn launch_client(executable: &Path, port: u16) -> Result<(), String> {
    let executable = validate_executable(executable)?;
    if port == 0 {
        return Err("调试端口必须是 1–65535".into());
    }
    #[cfg(windows)]
    {
        windows::launch(&executable, port)
    }
    #[cfg(not(windows))]
    {
        let _ = executable;
        Err("启动客户端仅支持 Windows".into())
    }
}
pub fn handle_helper_args() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|value| value != HELPER_FLAG) {
        return None;
    }
    if args.len() != 4 {
        return Some(1);
    }
    let port = args[3]
        .to_str()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|p| *p > 0);
    let Some(port) = port else {
        return Some(1);
    };
    #[cfg(windows)]
    {
        Some(windows::helper(Path::new(&args[2]), port))
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        Some(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "只在显式本机验证时读取真实注册表，不启动客户端"]
    fn discover_installed_client_read_only() {
        let installations = discover_installations().unwrap();
        for installation in &installations {
            validate_executable(Path::new(&installation.executable_path)).unwrap();
        }
        println!("{}", serde_json::to_string_pretty(&installations).unwrap());
    }

    #[test]
    fn client_command_strips_host_runtime_flags_and_keeps_debug_arguments() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("biubiu.exe");
        let command = client_command(&exe, 9444).unwrap();
        assert_eq!(command.get_program(), exe.as_os_str());
        assert_eq!(command.get_current_dir(), Some(root.path()));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert_eq!(
            args,
            [
                "--remote-debugging-port=9444",
                "--remote-debugging-address=127.0.0.1"
            ]
        );
        for variable in HOST_RUNTIME_VARIABLES {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == variable && value.is_none()),
                "未隔离 {variable}"
            );
        }
    }

    #[test]
    fn launch_reports_early_exit_without_stopping_other_processes() {
        const CHILD_EXIT: &str = "AUTOBIUBIU_TEST_CHILD_EXIT";
        if let Ok(code) = std::env::var(CHILD_EXIT) {
            std::process::exit(code.parse().unwrap());
        }
        // 仅重入当前单元测试的子进程，不执行真实客户端或管理员辅助模式。
        for code in [9, 0] {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "client_platform::tests::launch_reports_early_exit_without_stopping_other_processes"])
                .env(CHILD_EXIT, code.to_string())
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                .spawn().unwrap();
            let result = check_initial_exit(child);
            if code == 9 {
                assert!(result.unwrap_err().contains("退出码 9"));
            } else {
                assert!(result.is_ok());
            }
        }
    }

    #[test]
    fn icon_paths_ignore_index_and_never_include_arguments() {
        assert_eq!(
            icon_path(r#""C:\Program Files\biubiu\biubiu.exe",0"#),
            Some(PathBuf::from(r"C:\Program Files\biubiu\biubiu.exe"))
        );
        assert_eq!(
            icon_path(r"C:\biubiu\biubiu.exe,1"),
            Some(PathBuf::from(r"C:\biubiu\biubiu.exe"))
        );
        assert!(icon_path("missing").is_none());
    }
    #[test]
    fn argument_quoting_preserves_spaces_quotes_and_trailing_backslashes() {
        assert_eq!(quote_argument("a b"), "\"a b\"");
        assert_eq!(quote_argument("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_argument("a\\"), "\"a\\\\\"");
    }
    #[test]
    fn only_existing_biubiu_executable_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("biubiu.exe");
        assert!(validate_executable(&exe).is_err());
        std::fs::write(&exe, b"fixture").unwrap();
        assert!(validate_executable(&exe).is_ok());
        let other = root.path().join("other.exe");
        std::fs::write(&other, b"fixture").unwrap();
        assert!(validate_executable(&other).is_err());
    }
}
