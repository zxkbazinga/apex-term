use std::collections::HashMap;
use std::env;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use keyring::Entry;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use socket2::{SockRef, TcpKeepalive};
use ssh2::Channel;
use tauri::{AppHandle, Emitter, State};

const MAX_SFTP_FILE_SIZE: usize = 32 * 1024 * 1024;
const CREDENTIAL_SERVICE: &str = "com.zzbazinga.apex-term";

#[tauri::command]
fn app_exit(app: AppHandle) {
    app.exit(0);
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

fn resolve_address(host: &str, port: u16) -> Result<SocketAddr, String> {
    (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("无法解析主机地址：{error}"))?
        .next()
        .ok_or_else(|| "没有找到可用的主机地址".to_string())
}

fn connect_authenticated_ssh(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    trusted_fingerprint: Option<&str>,
) -> Result<(ssh2::Session, TcpStream), String> {
    let address = resolve_address(host, port)?;
    let socket = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .map_err(|error| format!("连接失败：{error}"))?;
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
    session
        .userauth_password(username, password)
        .map_err(|error| format!("SSH 认证失败：{error}"))?;
    if !session.authenticated() {
        return Err("SSH 认证失败，请检查用户名和密码".to_string());
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

fn with_sftp<T>(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    trusted_fingerprint: Option<&str>,
    operation: impl FnOnce(&ssh2::Sftp) -> Result<T, String>,
) -> Result<T, String> {
    let (session, _) =
        connect_authenticated_ssh(host, port, username, password, trusted_fingerprint)?;
    let sftp = session
        .sftp()
        .map_err(|error| format!("无法启动 SFTP 子系统：{error}"))?;
    operation(&sftp)
}

async fn run_sftp<T: Send + 'static>(
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    operation: impl FnOnce(&ssh2::Sftp) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        with_sftp(
            &host,
            port,
            &username,
            &password,
            trusted_fingerprint.as_deref(),
            operation,
        )
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
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    path: String,
) -> Result<SftpListing, String> {
    run_sftp(
        host,
        port,
        username,
        password,
        trusted_fingerprint,
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

#[tauri::command]
async fn sftp_upload(
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    path: String,
    data: Vec<u8>,
    overwrite: bool,
) -> Result<(), String> {
    if data.len() > MAX_SFTP_FILE_SIZE {
        return Err("单个文件最大支持 32 MB".to_string());
    }
    run_sftp(
        host,
        port,
        username,
        password,
        trusted_fingerprint,
        move |sftp| {
            if !overwrite {
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
            let mut file = sftp
                .create(Path::new(&path))
                .map_err(|error| {
                    if error.code() == ssh2::ErrorCode::SFTP(3) {
                        format!(
                            "没有权限在远程路径“{path}”创建文件。请切换到 SSH 用户有写入权限的目录（例如其 home 目录），或调整 Ubuntu 目录的所有者/权限。"
                        )
                    } else {
                        format!("创建远程文件失败：{error}")
                    }
                })?;
            file.write_all(&data)
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        format!(
                            "没有权限向远程路径“{path}”写入文件。请切换到 SSH 用户有写入权限的目录（例如其 home 目录），或调整 Ubuntu 目录的所有者/权限。"
                        )
                    } else {
                        format!("上传文件失败：{error}")
                    }
                })
        },
    )
    .await
}

#[tauri::command]
async fn sftp_download(
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    path: String,
) -> Result<Vec<u8>, String> {
    run_sftp(
        host,
        port,
        username,
        password,
        trusted_fingerprint,
        move |sftp| {
            let mut file = sftp
                .open(Path::new(&path))
                .map_err(|error| format!("打开远程文件失败：{error}"))?;
            let mut data = Vec::new();
            Read::by_ref(&mut file)
                .take((MAX_SFTP_FILE_SIZE + 1) as u64)
                .read_to_end(&mut data)
                .map_err(|error| format!("下载文件失败：{error}"))?;
            if data.len() > MAX_SFTP_FILE_SIZE {
                return Err("单个文件最大支持 32 MB".to_string());
            }
            Ok(data)
        },
    )
    .await
}

#[tauri::command]
async fn sftp_mkdir(
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    path: String,
) -> Result<(), String> {
    run_sftp(
        host,
        port,
        username,
        password,
        trusted_fingerprint,
        move |sftp| {
            sftp.mkdir(Path::new(&path), 0o755)
                .map_err(|error| format!("创建远程目录失败：{error}"))
        },
    )
    .await
}

#[tauri::command]
async fn sftp_delete(
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    path: String,
) -> Result<(), String> {
    run_sftp(
        host,
        port,
        username,
        password,
        trusted_fingerprint,
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
    session_id: String,
    host: String,
    port: u16,
    username: String,
    password: String,
    trusted_fingerprint: Option<String>,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let sessions = sessions.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (session, socket) = connect_authenticated_ssh(
            &host,
            port,
            &username,
            &password,
            trusted_fingerprint.as_deref(),
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

        let output_app = app.clone();
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
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (session, _) = connect_authenticated_ssh(
            &host,
            port,
            &username,
            &password,
            trusted_fingerprint.as_deref(),
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
fn ssh_disconnect(sessions: State<'_, SshSessions>, session_id: String) -> Result<(), String> {
    let channel = sessions
        .0
        .lock()
        .map_err(|_| "SSH 会话状态异常".to_string())?
        .remove(&session_id);
    if let Some(channel) = channel {
        let mut channel = channel.lock().map_err(|_| "SSH 终端状态异常".to_string())?;
        let _ = channel.close();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .enable_macos_default_menu(false)
        .manage(SshSessions::default())
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
            sftp_upload,
            sftp_download,
            sftp_mkdir,
            sftp_delete
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
