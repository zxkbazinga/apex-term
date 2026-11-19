use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use keyring::Entry;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use socket2::{SockRef, TcpKeepalive};
use ssh2::{Channel, OpenFlags, OpenType};
use tauri::{AppHandle, Emitter, Manager, State};

const SFTP_CHUNK_SIZE: usize = 1024 * 1024;
const MAX_UPDATE_DOWNLOAD_SIZE: u64 = 1024 * 1024 * 1024;
const CREDENTIAL_SERVICE: &str = "com.zzbazinga.apex-term";

#[tauri::command]
fn app_exit(app: AppHandle) {
    app.exit(0);
}

#[derive(Clone, Serialize)]
struct UpdateDownloadProgress {
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

fn update_download_path(
    download_dir: &Path,
    file_name: &str,
) -> Result<(PathBuf, PathBuf), String> {
    if file_name.is_empty()
        || file_name
            .chars()
            .any(|character| matches!(character, '/' | '\\' | ':') || character.is_control())
    {
        return Err("更新安装包文件名无效".to_string());
    }

    #[cfg(target_os = "macos")]
    let supported = file_name.to_lowercase().ends_with(".dmg");
    #[cfg(target_os = "windows")]
    let supported = {
        let name = file_name.to_lowercase();
        name.ends_with(".msi") || name.ends_with(".exe")
    };
    #[cfg(target_os = "linux")]
    let supported = {
        let name = file_name.to_lowercase();
        name.ends_with(".appimage") || name.ends_with(".deb") || name.ends_with(".rpm")
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let supported = false;
    if !supported {
        return Err("该安装包格式与当前系统不兼容".to_string());
    }

    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "更新安装包文件名无效".to_string())?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "更新安装包缺少文件扩展名".to_string())?;

    for suffix in 0..=999 {
        let name = if suffix == 0 {
            file_name.to_string()
        } else {
            format!("{stem} ({suffix}).{extension}")
        };
        let destination = download_dir.join(&name);
        let partial = download_dir.join(format!("{name}.part"));
        if !destination.exists() && !partial.exists() {
            return Ok((destination, partial));
        }
    }
    Err("下载目录中存在过多同名安装包".to_string())
}

fn is_allowed_update_host(host: &str) -> bool {
    matches!(
        host,
        "github.com"
            | "release-assets.githubusercontent.com"
            | "objects.githubusercontent.com"
            | "github-releases.githubusercontent.com"
    )
}

#[tauri::command]
async fn download_update(app: AppHandle, url: String, file_name: String) -> Result<String, String> {
    let asset_url = reqwest::Url::parse(&url).map_err(|error| format!("更新地址无效：{error}"))?;
    if asset_url.scheme() != "https"
        || asset_url.host_str() != Some("github.com")
        || !asset_url
            .path()
            .starts_with("/zxkbazinga/apex-term/releases/download/")
    {
        return Err("只允许下载 Apex Term 官方 GitHub Release 安装包".to_string());
    }

    let download_dir = app
        .path()
        .download_dir()
        .map_err(|error| format!("无法获取系统下载目录：{error}"))?;
    fs::create_dir_all(&download_dir).map_err(|error| format!("无法创建下载目录：{error}"))?;
    let (destination, partial) = update_download_path(&download_dir, &file_name)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30 * 60))
        .user_agent(format!("ApexTerm/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() < 10
                && url.scheme() == "https"
                && matches!(url.host_str(), Some(host) if is_allowed_update_host(host))
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|error| format!("无法初始化更新下载：{error}"))?;
    let mut response = client
        .get(asset_url)
        .header("Accept", "application/octet-stream")
        .send()
        .await
        .map_err(|error| format!("连接更新服务器失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("更新服务器返回 HTTP {}", response.status()));
    }
    if !response
        .url()
        .host_str()
        .is_some_and(is_allowed_update_host)
    {
        return Err("更新下载被重定向到非可信服务器".to_string());
    }

    let total_bytes = response.content_length();
    if total_bytes.is_some_and(|size| size > MAX_UPDATE_DOWNLOAD_SIZE) {
        return Err("更新安装包超过 1 GB，已取消下载".to_string());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| format!("无法写入下载文件：{error}"))?;
    let mut downloaded_bytes = 0_u64;
    let mut last_progress = Instant::now();
    let download_result = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("下载更新包失败：{error}"))?
        {
            downloaded_bytes += chunk.len() as u64;
            if downloaded_bytes > MAX_UPDATE_DOWNLOAD_SIZE {
                return Err("更新安装包超过 1 GB，已取消下载".to_string());
            }
            file.write_all(&chunk)
                .map_err(|error| format!("保存更新安装包失败：{error}"))?;
            if last_progress.elapsed() >= Duration::from_millis(200) {
                let _ = app.emit(
                    "update-download-progress",
                    UpdateDownloadProgress {
                        downloaded_bytes,
                        total_bytes,
                    },
                );
                last_progress = Instant::now();
            }
        }
        file.flush()
            .map_err(|error| format!("保存更新安装包失败：{error}"))?;
        if downloaded_bytes == 0 {
            return Err("更新服务器返回了空安装包".to_string());
        }
        Ok(())
    }
    .await;
    drop(file);
    if let Err(error) = download_result {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }
    #[cfg(target_os = "linux")]
    if file_name.to_lowercase().ends_with(".appimage") {
        use std::os::unix::fs::PermissionsExt;

        if let Err(error) = fs::set_permissions(&partial, fs::Permissions::from_mode(0o755)) {
            let _ = fs::remove_file(&partial);
            return Err(format!("无法设置 AppImage 执行权限：{error}"));
        }
    }
    if let Err(error) = fs::rename(&partial, &destination) {
        let _ = fs::remove_file(&partial);
        return Err(format!("无法完成更新安装包保存：{error}"));
    }
    let _ = app.emit(
        "update-download-progress",
        UpdateDownloadProgress {
            downloaded_bytes,
            total_bytes,
        },
    );
    Ok(destination.to_string_lossy().into_owned())
}

fn credential_entry(profile_id: &str) -> Result<Entry, String> {
    Entry::new(CREDENTIAL_SERVICE, profile_id)
        .map_err(|error| format!("无法访问系统凭据管理器：{error}"))
}

#[tauri::command]
fn credential_store(profile_id: String, password: String) -> Result<(), String> {
    credential_entry(&profile_id)?
        .set_password(&password)
        .map_err(|error| format!("保存 SSH 密码到系统凭据管理器失败：{error}"))
}

#[tauri::command]
fn credential_get(profile_id: String) -> Result<Option<String>, String> {
    match credential_entry(&profile_id)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("读取系统凭据管理器中的 SSH 密码失败：{error}")),
    }
}

#[tauri::command]
fn credential_delete(profile_id: String) -> Result<(), String> {
    match credential_entry(&profile_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("从系统凭据管理器删除 SSH 密码失败：{error}")),
    }
}

#[derive(Default)]
struct SshSessions(Arc<Mutex<HashMap<String, Arc<Mutex<Channel>>>>>);

struct SftpSession {
    _session: ssh2::Session,
    sftp: ssh2::Sftp,
}

/// 建立 SFTP 所需的认证参数，在 SSH 连接成功时保存；
/// 真正建立 SFTP 连接推迟到首次使用文件管理时（按需建立）。
struct SftpConnectionParams {
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    auth_method: String,
    private_key: Option<String>,
    passphrase: Option<String>,
}

/// 每个 SSH 会话对应一份 SFTP 状态：参数 + 惰性建立的连接。
struct SftpSessionState {
    params: SftpConnectionParams,
    session: Option<Arc<Mutex<SftpSession>>>,
}

#[derive(Default)]
struct SftpSessions(Arc<Mutex<HashMap<String, Arc<Mutex<SftpSessionState>>>>>);

struct LocalSession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send>,
}

#[derive(Default)]
struct LocalSessions(Arc<Mutex<HashMap<String, Arc<Mutex<LocalSession>>>>>);

#[cfg(target_os = "macos")]
fn local_shell_command() -> CommandBuilder {
    let mut command =
        CommandBuilder::new(env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()));
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("LANG", "en_US.UTF-8");
    command
}

#[cfg(all(unix, not(target_os = "macos")))]
fn local_shell_command() -> CommandBuilder {
    let mut command =
        CommandBuilder::new(env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()));
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("LANG", "en_US.UTF-8");
    command
}

#[cfg(target_os = "windows")]
fn local_shell_command() -> CommandBuilder {
    let mut command =
        CommandBuilder::new(env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()));
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command
}

fn emit_utf8_chunk(app: &AppHandle, event_name: &str, pending: &mut Vec<u8>, chunk: &[u8]) {
    pending.extend_from_slice(chunk);
    match std::str::from_utf8(pending) {
        Ok(output) => {
            if !output.is_empty() {
                let _ = app.emit(event_name, output.to_string());
            }
            pending.clear();
        }
        Err(error) if error.error_len().is_none() => {
            let valid_length = error.valid_up_to();
            if valid_length > 0 {
                let output =
                    std::str::from_utf8(&pending[..valid_length]).expect("valid UTF-8 prefix");
                let _ = app.emit(event_name, output.to_string());
                pending.drain(..valid_length);
            }
        }
        Err(_) => {
            let output = String::from_utf8_lossy(pending).into_owned();
            let _ = app.emit(event_name, output);
            pending.clear();
        }
    }
}

fn local_working_directory() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[tauri::command]
fn local_start(
    app: AppHandle,
    sessions: State<'_, LocalSessions>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("创建本地终端失败：{error}"))?;

    let mut command = local_shell_command();
    command.cwd(local_working_directory());
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("启动本地 Shell 失败：{error}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("读取本地终端失败：{error}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| format!("打开本地终端输入失败：{error}"))?;
    let session = Arc::new(Mutex::new(LocalSession {
        writer,
        master: pair.master,
        child,
    }));
    sessions
        .0
        .lock()
        .map_err(|_| "本地终端会话状态异常".to_string())?
        .insert(session_id.clone(), session);

    let sessions = sessions.0.clone();
    thread::spawn(move || {
        let event_name = format!("local-output-{session_id}");
        let mut buffer = [0_u8; 8192];
        let mut pending = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    if !pending.is_empty() {
                        let output = String::from_utf8_lossy(&pending).into_owned();
                        let _ = app.emit(&event_name, output);
                    }
                    break;
                }
                Ok(length) => {
                    emit_utf8_chunk(&app, &event_name, &mut pending, &buffer[..length]);
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = app.emit(&event_name, format!("\r\n[本地终端读取错误：{error}]\r\n"));
                    break;
                }
            }
        }
        if let Ok(mut sessions) = sessions.lock() {
            sessions.remove(&session_id);
        }
        let ended_event_name = format!("local-session-ended-{session_id}");
        let _ = app.emit(&ended_event_name, "本地 Shell 已退出");
    });
    Ok(())
}

#[tauri::command]
fn local_write(
    sessions: State<'_, LocalSessions>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    let session = sessions
        .0
        .lock()
        .map_err(|_| "本地终端会话状态异常".to_string())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "本地终端会话不存在或已关闭".to_string())?;
    let mut session = session.lock().map_err(|_| "本地终端状态异常".to_string())?;
    session
        .writer
        .write_all(data.as_bytes())
        .map_err(|error| format!("发送本地终端输入失败：{error}"))?;
    session
        .writer
        .flush()
        .map_err(|error| format!("刷新本地终端失败：{error}"))
}

#[tauri::command]
fn local_resize(
    sessions: State<'_, LocalSessions>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let session = sessions
        .0
        .lock()
        .map_err(|_| "本地终端会话状态异常".to_string())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "本地终端会话不存在或已关闭".to_string())?;
    session
        .lock()
        .map_err(|_| "本地终端状态异常".to_string())?
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("调整本地终端尺寸失败：{error}"))
}

#[tauri::command]
fn local_stop(sessions: State<'_, LocalSessions>, session_id: String) -> Result<(), String> {
    let session = sessions
        .0
        .lock()
        .map_err(|_| "本地终端会话状态异常".to_string())?
        .remove(&session_id);
    if let Some(session) = session {
        let mut session = session.lock().map_err(|_| "本地终端状态异常".to_string())?;
        session
            .child
            .kill()
            .map_err(|error| format!("关闭本地 Shell 失败：{error}"))?;
    }
    Ok(())
}

fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("无法解析主机地址：{error}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("没有找到可用的主机地址".to_string());
    }
    let mut last_error: Option<std::io::Error> = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
            Ok(socket) => return Ok(socket),
            Err(error) => last_error = Some(error),
        }
    }
    Err(format!(
        "连接失败：{}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "没有可用的主机地址".to_string())
    ))
}

fn connect_authenticated_ssh(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    trusted_fingerprint: Option<&str>,
    auth_method: &str,
    private_key: Option<&str>,
    passphrase: Option<&str>,
) -> Result<(ssh2::Session, TcpStream), String> {
    let socket = connect_tcp(host, port)?;
    SockRef::from(&socket)
        .set_tcp_keepalive(
            &TcpKeepalive::new()
                .with_time(Duration::from_secs(15))
                .with_interval(Duration::from_secs(5))
                .with_retries(3),
        )
        .map_err(|error| format!("配置 SSH 心跳检测失败：{error}"))?;
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    let timeout_reset_socket = socket
        .try_clone()
        .map_err(|error| format!("复制 SSH socket 失败：{error}"))?;

    let mut session = ssh2::Session::new().map_err(|error| error.to_string())?;
    session.set_tcp_stream(socket);
    session
        .method_pref(
            ssh2::MethodType::Kex,
            "curve25519-sha256,curve25519-sha256@libssh.org,ecdh-sha2-nistp256,ecdh-sha2-nistp384,ecdh-sha2-nistp521,diffie-hellman-group-exchange-sha256,diffie-hellman-group16-sha512,diffie-hellman-group18-sha512,diffie-hellman-group14-sha256,diffie-hellman-group14-sha1,diffie-hellman-group-exchange-sha1,diffie-hellman-group1-sha1",
        )
        .map_err(|error| format!("配置 SSH 密钥交换算法失败：{error}"))?;
    session
        .method_pref(
            ssh2::MethodType::HostKey,
            "ssh-ed25519,ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,rsa-sha2-512,rsa-sha2-256,ssh-rsa",
        )
        .map_err(|error| format!("配置 SSH 主机密钥算法失败：{error}"))?;
    for direction in [ssh2::MethodType::CryptCs, ssh2::MethodType::CryptSc] {
        session
            .method_pref(
                direction,
                "aes256-ctr,aes192-ctr,aes128-ctr,aes256-cbc,aes192-cbc,aes128-cbc",
            )
            .map_err(|error| format!("配置 SSH 加密算法失败：{error}"))?;
    }
    for direction in [ssh2::MethodType::MacCs, ssh2::MethodType::MacSc] {
        session
            .method_pref(
                direction,
                "hmac-sha2-512-etm@openssh.com,hmac-sha2-256-etm@openssh.com,hmac-sha2-512,hmac-sha2-256,hmac-sha1-etm@openssh.com,hmac-sha1",
            )
            .map_err(|error| format!("配置 SSH 消息认证算法失败：{error}"))?;
    }
    session.handshake().map_err(|error| {
        if error.code() == ssh2::ErrorCode::Session(-5) {
            format!(
                "SSH 算法协商失败：{error}。客户端已启用现代算法及有限的旧版兼容算法，但仍未能与服务端找到共同算法。请确认主机和端口正确，并检查服务端 sshd 的 KexAlgorithms、HostKeyAlgorithms 设置。"
            )
        } else {
            format!("SSH 握手失败：{error}")
        }
    })?;
    let fingerprint = session
        .host_key_hash(ssh2::HashType::Sha256)
        .ok_or_else(|| "无法读取 SSH 主机指纹".to_string())?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let fingerprint = format!("SHA256:{fingerprint}");
    match trusted_fingerprint {
        Some(trusted) if trusted == fingerprint => {}
        Some(_) => return Err(format!("HOST_KEY_CHANGED:{fingerprint}")),
        None => return Err(format!("HOST_KEY_CONFIRM:{fingerprint}")),
    }
    if auth_method == "privateKey" {
        let key = private_key.ok_or_else(|| "未提供私钥内容".to_string())?;
        let pass = passphrase.filter(|value| !value.is_empty());
        session
            .userauth_pubkey_memory(username, None, key, pass)
            .map_err(|error| format!("SSH 私钥认证失败：{error}。请确认私钥格式正确，若为加密私钥请填写口令。"))?;
    } else {
        session
            .userauth_password(username, password)
            .map_err(|error| format!("SSH 密码认证失败：{error}"))?;
    }
    if !session.authenticated() {
        return Err("SSH 认证失败，请检查用户名和凭据".to_string());
    }
    timeout_reset_socket
        .set_read_timeout(None)
        .map_err(|error| format!("清除 SSH 读取超时失败：{error}"))?;
    timeout_reset_socket
        .set_write_timeout(None)
        .map_err(|error| format!("清除 SSH 写入超时失败：{error}"))?;
    Ok((session, timeout_reset_socket))
}

fn write_channel_all(channel: &mut Channel, mut data: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !data.is_empty() {
        match channel.write(data) {
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::WriteZero,
                    "SSH 终端通道未能写入数据",
                ));
            }
            Ok(written) => data = &data[written..],
            Err(error) if error.kind() == ErrorKind::WouldBlock => wait_for_ssh_retry(deadline)?,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn wait_for_ssh_retry(deadline: Instant) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(ErrorKind::TimedOut, "SSH 终端通道操作超时"));
    }
    thread::sleep(remaining.min(Duration::from_millis(12)));
    Ok(())
}

fn resize_channel(channel: &mut Channel, cols: u32, rows: u32) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match channel.request_pty_size(cols, rows, None, None) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let error = io::Error::from(error);
                match error.kind() {
                    ErrorKind::WouldBlock => wait_for_ssh_retry(deadline)
                        .map_err(|retry_error| format!("调整终端尺寸失败：{retry_error}"))?,
                    ErrorKind::Interrupted => {}
                    _ => return Err(format!("调整终端尺寸失败：{error}")),
                }
            }
        }
    }
}

type SftpSessionMap = Arc<Mutex<HashMap<String, Arc<Mutex<SftpSessionState>>>>>;

fn establish_sftp(params: &SftpConnectionParams) -> Result<SftpSession, String> {
    let (session, _) = connect_authenticated_ssh(
        &params.host,
        params.port,
        &params.username,
        &params.password,
        params.trusted_fingerprint.as_deref(),
        &params.auth_method,
        params.private_key.as_deref(),
        params.passphrase.as_deref(),
    )?;
    let sftp = session
        .sftp()
        .map_err(|error| format!("无法启动 SFTP 子系统：{error}"))?;
    Ok(SftpSession {
        _session: session,
        sftp,
    })
}

async fn run_cached_sftp<T: Send + 'static>(
    sessions: SftpSessionMap,
    session_id: String,
    operation: impl FnOnce(&ssh2::Sftp) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = sessions
            .lock()
            .map_err(|_| "SFTP 会话状态异常".to_string())?
            .get(&session_id)
            .cloned()
            .ok_or_else(|| {
                "当前 SSH 会话不支持 SFTP，或连接已断开。请确认服务器已开启 SFTP 子系统。"
                    .to_string()
            })?;

        let mut state = state.lock().map_err(|_| "SFTP 会话状态异常".to_string())?;
        if state.session.is_none() {
            state.session = Some(Arc::new(Mutex::new(establish_sftp(&state.params)?)));
        }
        let handle = state.session.as_ref().unwrap().clone();
        drop(state);

        let guard = handle.lock().map_err(|_| "SFTP 会话状态异常".to_string())?;
        operation(&guard.sftp)
    })
    .await
    .map_err(|error| format!("SFTP 任务失败：{error}"))?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SftpEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SftpListing {
    path: String,
    entries: Vec<SftpEntry>,
}

#[tauri::command]
async fn sftp_list(
    sessions: State<'_, SftpSessions>,
    session_id: String,
    path: String,
) -> Result<SftpListing, String> {
    run_cached_sftp(
        sessions.0.clone(),
        session_id,
        move |sftp| {
            let directory = sftp
                .realpath(Path::new(&path))
                .map_err(|error| format!("无法打开远程目录：{error}"))?;
            let directory_string = directory.to_string_lossy().into_owned();
            let mut entries = sftp
                .readdir(&directory)
                .map_err(|error| format!("读取远程目录失败：{error}"))?
                .into_iter()
                .filter_map(|(entry_path, stat)| {
                    let name = entry_path.file_name()?.to_string_lossy().into_owned();
                    if name == "." || name == ".." {
                        return None;
                    }
                    let base = directory_string.trim_end_matches('/');
                    let entry_path = if base.is_empty() {
                        format!("/{name}")
                    } else {
                        format!("{base}/{name}")
                    };
                    Some(SftpEntry {
                        name,
                        path: entry_path,
                        is_dir: stat.is_dir(),
                        size: stat.size,
                        modified: stat.mtime,
                    })
                })
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| {
                right
                    .is_dir
                    .cmp(&left.is_dir)
                    .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            });
            Ok(SftpListing {
                path: directory_string,
                entries,
            })
        },
    )
    .await
}

/// 分块上传：`create` 为首块（新建并截断文件、做覆盖检查），后续块以写模式打开并 seek 到 offset。
/// 每块独立开/关文件句柄，前端逐块读取文件，因此内存占用恒定，不再有 32 MB 上限。
#[tauri::command]
async fn sftp_upload_chunk(
    sessions: State<'_, SftpSessions>,
    session_id: String,
    path: String,
    offset: u64,
    data: Vec<u8>,
    overwrite: bool,
    create: bool,
) -> Result<(), String> {
    if data.len() > SFTP_CHUNK_SIZE {
        return Err(format!(
            "单个数据块不能超过 {} MB",
            SFTP_CHUNK_SIZE / 1024 / 1024
        ));
    }
    run_cached_sftp(
        sessions.0.clone(),
        session_id,
        move |sftp| {
            if create && !overwrite {
                match sftp.stat(Path::new(&path)) {
                    Ok(_) => return Err("远程文件已存在，请确认覆盖后重试".to_string()),
                    Err(error)
                        if matches!(
                            error.code(),
                            ssh2::ErrorCode::SFTP(2) | ssh2::ErrorCode::SFTP(10)
                        ) => {}
                    Err(error) => return Err(format!("检查远程文件失败：{error}")),
                }
            }
            let mut file = if create {
                sftp.create(Path::new(&path))
            } else {
                sftp.open_mode(
                    Path::new(&path),
                    OpenFlags::WRITE,
                    0o644,
                    OpenType::File,
                )
            }
            .map_err(|error| {
                if error.code() == ssh2::ErrorCode::SFTP(3) {
                    format!(
                        "没有权限在远程路径“{path}”创建文件。请切换到 SSH 用户有写入权限的目录（例如其 home 目录），或调整目录的所有者/权限。"
                    )
                } else {
                    format!("打开远程文件失败：{error}")
                }
            })?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| format!("定位远程文件失败：{error}"))?;
            file.write_all(&data)
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        format!(
                            "没有权限向远程路径“{path}”写入文件。请切换到 SSH 用户有写入权限的目录（例如其 home 目录），或调整目录的所有者/权限。"
                        )
                    } else {
                        format!("上传文件失败：{error}")
                    }
                })
        },
    )
    .await
}

/// 分块下载：按 offset 读取一块（最多 1 MB），前端逐块拼接成 Blob。
/// 每次只把一块经 IPC 传回，内存占用恒定，不再有 32 MB 上限。
#[tauri::command]
async fn sftp_download_chunk(
    sessions: State<'_, SftpSessions>,
    session_id: String,
    path: String,
    offset: u64,
) -> Result<Vec<u8>, String> {
    run_cached_sftp(
        sessions.0.clone(),
        session_id,
        move |sftp| {
            let mut file = sftp
                .open(Path::new(&path))
                .map_err(|error| format!("打开远程文件失败：{error}"))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| format!("定位远程文件失败：{error}"))?;
            let mut data = Vec::with_capacity(SFTP_CHUNK_SIZE);
            Read::by_ref(&mut file)
                .take(SFTP_CHUNK_SIZE as u64)
                .read_to_end(&mut data)
                .map_err(|error| format!("下载文件失败：{error}"))?;
            Ok(data)
        },
    )
    .await
}

#[tauri::command]
async fn sftp_mkdir(
    sessions: State<'_, SftpSessions>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    run_cached_sftp(
        sessions.0.clone(),
        session_id,
        move |sftp| {
            sftp.mkdir(Path::new(&path), 0o755)
                .map_err(|error| format!("创建远程目录失败：{error}"))
        },
    )
    .await
}

#[tauri::command]
async fn sftp_delete(
    sessions: State<'_, SftpSessions>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    run_cached_sftp(
        sessions.0.clone(),
        session_id,
        move |sftp| {
            let stat = sftp
                .stat(Path::new(&path))
                .map_err(|error| format!("读取远程文件信息失败：{error}"))?;
            if stat.is_dir() {
                sftp.rmdir(Path::new(&path))
                    .map_err(|error| format!("删除远程目录失败（目录需为空）：{error}"))
            } else {
                sftp.unlink(Path::new(&path))
                    .map_err(|error| format!("删除远程文件失败：{error}"))
            }
        },
    )
    .await
}

#[tauri::command]
async fn ssh_connect(
    app: AppHandle,
    sessions: State<'_, SshSessions>,
    sftp_sessions: State<'_, SftpSessions>,
    session_id: String,
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    auth_method: Option<String>,
    private_key: Option<String>,
    passphrase: Option<String>,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let sessions = sessions.0.clone();
    let sftp_sessions = sftp_sessions.0.clone();
    let auth_method_value = auth_method.unwrap_or_else(|| "password".to_string());
    tauri::async_runtime::spawn_blocking(move || {
        let (session, socket) = connect_authenticated_ssh(
            &host,
            port,
            &username,
            &password,
            trusted_fingerprint.as_deref(),
            &auth_method_value,
            private_key.as_deref(),
            passphrase.as_deref(),
        )?;

        let mut channel = session
            .channel_session()
            .map_err(|error| format!("创建终端失败：{error}"))?;
        channel
            .request_pty("xterm-256color", None, Some((cols, rows, 0, 0)))
            .map_err(|error| format!("申请终端失败：{error}"))?;
        channel
            .shell()
            .map_err(|error| format!("启动 Shell 失败：{error}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|error| format!("配置 SSH 终端非阻塞 socket 失败：{error}"))?;
        session.set_blocking(false);

        let channel = Arc::new(Mutex::new(channel));
        let diagnostic_socket = socket
            .try_clone()
            .map_err(|error| format!("复制 SSH 诊断 socket 失败：{error}"))?;
        sessions
            .lock()
            .map_err(|_| "SSH 会话状态异常".to_string())?
            .insert(session_id.clone(), channel.clone());

        // 只保存建立 SFTP 所需的认证参数，不在这里真正建立连接。
        // 真正连接推迟到用户打开 SFTP 面板时（见 run_cached_sftp），这样：
        //   1) 不支持 SFTP 子系统的服务器（如群晖 NAS）仍能正常使用 SSH 终端；
        //   2) SSH 连接不再额外多做一次完整的 TCP + 认证握手；
        //   3) SFTP 失败只在文件面板内提示，不再污染终端输出。
        if let Ok(mut map) = sftp_sessions.lock() {
            map.insert(
                session_id.clone(),
                Arc::new(Mutex::new(SftpSessionState {
                    params: SftpConnectionParams {
                        host,
                        port,
                        username,
                        password,
                        trusted_fingerprint,
                        auth_method: auth_method_value,
                        private_key,
                        passphrase,
                    },
                    session: None,
                })),
            );
        }

        let output_app = app.clone();
        let sftp_sessions_cleanup = sftp_sessions.clone();
        thread::spawn(move || {
            let event_name = format!("ssh-output-{session_id}");
            let ended_event_name = format!("ssh-session-ended-{session_id}");
            let mut buffer = [0_u8; 8192];
            let mut pending = Vec::new();
            let session_is_current = || {
                sessions
                    .lock()
                    .map(|sessions| {
                        sessions
                            .get(&session_id)
                            .is_some_and(|current| Arc::ptr_eq(current, &channel))
                    })
                    .unwrap_or(false)
            };
            let end_reason = loop {
                if !session_is_current() {
                    break "SSH 会话已关闭或被替换".to_string();
                }
                let result = match channel.lock() {
                    Ok(mut channel) => channel.read(&mut buffer),
                    Err(_) => break "SSH 终端状态锁异常".to_string(),
                };
                match result {
                    Ok(0) => {
                        if !session_is_current() {
                            break "SSH 会话已关闭或被替换".to_string();
                        }
                        if !pending.is_empty() {
                            let output = String::from_utf8_lossy(&pending).into_owned();
                            let _ = output_app.emit(&event_name, output);
                        }
                        let _ = output_app.emit(&event_name, "\r\n\r\n[连接已关闭]\r\n");
                        break "远端关闭了 SSH 终端通道".to_string();
                    }
                    Ok(length) => {
                        if !session_is_current() {
                            break "SSH 会话已关闭或被替换".to_string();
                        }
                        emit_utf8_chunk(&output_app, &event_name, &mut pending, &buffer[..length]);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::WouldBlock | ErrorKind::Interrupted
                        ) =>
                    {
                        let mut probe = [0_u8; 1];
                        match diagnostic_socket.peek(&mut probe) {
                            Ok(0) => {
                                let _ =
                                    output_app.emit(&event_name, "\r\n\r\n[连接已关闭]\r\n");
                                break "SSH 对端关闭了 TCP 连接".to_string();
                            }
                            Ok(_) => continue,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    ErrorKind::WouldBlock | ErrorKind::Interrupted
                                ) =>
                            {
                                thread::sleep(Duration::from_millis(12));
                            }
                            Err(error) => {
                                if !session_is_current() {
                                    break "SSH 会话已关闭或被替换".to_string();
                                }
                                break format!("检查 SSH socket 状态失败：{error}");
                            }
                        }
                    }
                    Err(error) => {
                        if !session_is_current() {
                            break "SSH 会话已关闭或被替换".to_string();
                        }
                        let _ = output_app.emit(&event_name, "\r\n\r\n[SSH 连接中断]\r\n");
                        let mut probe = [0_u8; 1];
                        let socket_state = match diagnostic_socket.peek(&mut probe) {
                            Ok(0) => "收到 TCP EOF".to_string(),
                            Ok(bytes) => format!("仍有 {bytes} 字节可读"),
                            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                                "当前无可读数据".to_string()
                            }
                            Err(error) => format!("socket peek 错误：{error}"),
                        };
                        let socket_error = match diagnostic_socket.take_error() {
                            Ok(Some(error)) => error.to_string(),
                            Ok(None) => "无 socket 错误".to_string(),
                            Err(error) => format!("读取 socket 错误失败：{error}"),
                        };
                        break format!(
                            "SSH transport read 失败：{error}（{:?}）；socket 状态：{socket_state}；socket 错误：{socket_error}",
                            error.kind(),
                        );
                    }
                }
            };
            let is_current = if let Ok(mut sessions) = sessions.lock() {
                if sessions
                    .get(&session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &channel))
                {
                    sessions.remove(&session_id);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if is_current {
                if let Ok(mut map) = sftp_sessions_cleanup.lock() {
                    map.remove(&session_id);
                }
                let _ = output_app.emit(&ended_event_name, end_reason);
            }
        });
        Ok(())
    })
    .await
    .map_err(|error| format!("SSH 连接任务失败：{error}"))?
}

#[tauri::command]
async fn ssh_test_connection(
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    auth_method: Option<String>,
    private_key: Option<String>,
    passphrase: Option<String>,
) -> Result<(), String> {
    let auth_method_value = auth_method.unwrap_or_else(|| "password".to_string());
    tauri::async_runtime::spawn_blocking(move || {
        let (session, _) = connect_authenticated_ssh(
            &host,
            port,
            &username,
            &password,
            trusted_fingerprint.as_deref(),
            &auth_method_value,
            private_key.as_deref(),
            passphrase.as_deref(),
        )?;
        session
            .disconnect(None, "connection test complete", None)
            .map_err(|error| format!("关闭测试连接失败：{error}"))?;
        Ok(())
    })
    .await
    .map_err(|error| format!("SSH 测试连接任务失败：{error}"))?
}

#[tauri::command]
fn ssh_write(
    sessions: State<'_, SshSessions>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    let channel = sessions
        .0
        .lock()
        .map_err(|_| "SSH 会话状态异常".to_string())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "SSH 会话不存在或已关闭".to_string())?;
    let mut channel = channel.lock().map_err(|_| "SSH 终端状态异常".to_string())?;
    write_channel_all(&mut channel, data.as_bytes())
        .map_err(|error| format!("发送数据失败：{error}"))
}

#[tauri::command]
fn ssh_resize(
    sessions: State<'_, SshSessions>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let channel = sessions
        .0
        .lock()
        .map_err(|_| "SSH 会话状态异常".to_string())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "SSH 会话不存在或已关闭".to_string())?;
    let mut channel = channel.lock().map_err(|_| "SSH 终端状态异常".to_string())?;
    resize_channel(&mut channel, cols, rows)
}

#[tauri::command]
fn ssh_disconnect(
    sessions: State<'_, SshSessions>,
    sftp_sessions: State<'_, SftpSessions>,
    session_id: String,
) -> Result<(), String> {
    let channel = sessions
        .0
        .lock()
        .map_err(|_| "SSH 会话状态异常".to_string())?
        .remove(&session_id);
    if let Some(channel) = channel {
        let mut channel = channel.lock().map_err(|_| "SSH 终端状态异常".to_string())?;
        let _ = channel.close();
    }
    if let Ok(mut map) = sftp_sessions.0.lock() {
        map.remove(&session_id);
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .enable_macos_default_menu(false)
        .manage(SshSessions::default())
        .manage(SftpSessions::default())
        .manage(LocalSessions::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|_app| {
            #[cfg(target_os = "linux")]
            {
                let app = _app;
                use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};

                let appearance = SubmenuBuilder::new(app, "外观")
                    .item(&MenuItemBuilder::with_id("open-settings", "外观").build(app)?)
                    .build()?;
                let sessions = SubmenuBuilder::new(app, "会话")
                    .item(&MenuItemBuilder::with_id("open-local-terminal", "本机终端").build(app)?)
                    .item(&MenuItemBuilder::with_id("manage-sessions", "会话列表").build(app)?)
                    .item(&MenuItemBuilder::with_id("add-session", "添加 SSH 主机…").build(app)?)
                    .build()?;
                let sftp = SubmenuBuilder::new(app, "SFTP")
                    .item(&MenuItemBuilder::with_id("open-sftp", "打开 SFTP 文件…").build(app)?)
                    .build()?;
                let quit = MenuItemBuilder::with_id("quit-app", "退出 Apex Term")
                    .accelerator("CmdOrCtrl+Q")
                    .build(app)?;
                let about = SubmenuBuilder::new(app, "关于")
                    .item(&MenuItemBuilder::with_id("open-about", "关于 Apex Term").build(app)?)
                    .build()?;
                let menu = MenuBuilder::new(app)
                    .item(&quit)
                    .item(&appearance)
                    .item(&sessions)
                    .item(&sftp)
                    .item(&about)
                    .build()?;
                app.set_menu(menu)?;
            }
            Ok(())
        })
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "quit-app" {
                app.exit(0);
            } else {
                let _ = app.emit("native-menu-action", event.id().as_ref());
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_exit,
            download_update,
            credential_store,
            credential_get,
            credential_delete,
            local_start,
            local_write,
            local_resize,
            local_stop,
            ssh_connect,
            ssh_test_connection,
            ssh_write,
            ssh_resize,
            ssh_disconnect,
            sftp_list,
            sftp_upload_chunk,
            sftp_download_chunk,
            sftp_mkdir,
            sftp_delete
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
