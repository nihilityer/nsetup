//! Docker 与 Docker Compose 命令封装。

use crate::constants::{COMPOSE_FILE, ENV_FILE};
use crate::services::process;
use anyhow::Context;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// 执行 `docker compose up -d`
pub fn compose_up(app_dir: &Path) -> anyhow::Result<()> {
    run_compose(app_dir, &["up", "-d"]).map(|_| ())
}

/// 执行 `docker compose down`
pub fn compose_down(app_dir: &Path, remove_volumes: bool) -> anyhow::Result<()> {
    if remove_volumes {
        run_compose(app_dir, &["down", "--volumes"]).map(|_| ())
    } else {
        run_compose(app_dir, &["down"]).map(|_| ())
    }
}

/// 执行 `docker compose stop`
pub fn compose_stop(app_dir: &Path) -> anyhow::Result<()> {
    run_compose(app_dir, &["stop"]).map(|_| ())
}

/// 执行 `docker compose restart`
pub fn compose_restart(app_dir: &Path) -> anyhow::Result<()> {
    run_compose(app_dir, &["restart"]).map(|_| ())
}

/// Docker Compose 报告的镜像拉取进度。
#[derive(Debug, Clone)]
pub struct PullProgress {
    /// Docker 进度节点标识。
    pub id: String,
    /// 节点状态。
    pub status: String,
    /// 操作说明。
    pub text: String,
    /// 当前已处理的字节数。
    pub current: u64,
    /// 总字节数。
    pub total: u64,
}

/// 执行 `docker compose pull` 并逐项报告结构化进度。
pub fn compose_pull(
    app_dir: &Path,
    mut report: impl FnMut(PullProgress) -> bool,
    mut connected: impl FnMut() -> bool,
) -> anyhow::Result<()> {
    let mut command = compose_command(app_dir)?;
    let mut child = command
        .args(["--progress", "json", "pull"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("无法执行 Docker Compose: {}", app_dir.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 Docker Compose 标准输出"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 Docker Compose 标准错误"))?;
    let (sender, receiver) = mpsc::channel();
    let mut detail = String::new();
    let mut disconnected = false;

    std::thread::scope(|scope| -> anyhow::Result<()> {
        let stdout_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if stdout_sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if stderr_sender.send(line).is_err() {
                    break;
                }
            }
        });
        drop(sender);
        loop {
            if !connected() {
                disconnected = true;
                if child.try_wait()?.is_none() {
                    child.kill().context("无法终止已断开连接的镜像拉取")?;
                }
                break;
            }
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let line = line.context("无法读取 Docker Compose 进度")?;
                    if let Some(progress) = parse_pull_progress(&line)? {
                        if !report(progress) {
                            disconnected = true;
                            if child.try_wait()?.is_none() {
                                child.kill().context("无法终止已断开连接的镜像拉取")?;
                            }
                            break;
                        }
                    } else {
                        append_detail(&mut detail, &line);
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    })?;

    let status = child.wait().context("无法等待 Docker Compose 完成")?;
    if disconnected {
        return Ok(());
    }
    if !status.success() {
        anyhow::bail!(
            "docker compose pull 执行失败，退出码 {:?}: {}",
            status.code(),
            detail.trim()
        );
    }
    Ok(())
}

/// 执行 `docker compose build`
pub fn compose_build(app_dir: &Path) -> anyhow::Result<()> {
    run_compose(app_dir, &["build"]).map(|_| ())
}

/// 执行 `docker compose ps` 并返回逐行 JSON
pub fn compose_ps_json(app_dir: &Path) -> anyhow::Result<String> {
    run_compose(app_dir, &["ps", "--format", "json"])
}

/// 按行获取 Compose 项目日志，并可持续跟随新增内容。
pub fn compose_logs(
    app_dir: &Path,
    tail: u32,
    follow: bool,
    mut report: impl FnMut(String) -> bool,
    mut connected: impl FnMut() -> bool,
) -> anyhow::Result<()> {
    let tail = tail.clamp(1, 10_000).to_string();
    let mut command = compose_command(app_dir)?;
    command.args(["logs", "--no-color", "--tail", &tail]);
    if follow {
        command.arg("--follow");
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("无法执行 Docker Compose: {}", app_dir.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 Docker Compose 标准输出"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 Docker Compose 标准错误"))?;
    let (sender, receiver) = mpsc::channel();
    let mut detail = String::new();
    let mut disconnected = false;

    std::thread::scope(|scope| -> anyhow::Result<()> {
        let stdout_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if stdout_sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if stderr_sender.send(line).is_err() {
                    break;
                }
            }
        });
        drop(sender);
        loop {
            if !connected() {
                disconnected = true;
                terminate_child(&mut child, "日志跟随")?;
                break;
            }
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let line = line.context("无法读取 Docker Compose 日志")?;
                    append_detail(&mut detail, &line);
                    if !report(line) {
                        disconnected = true;
                        terminate_child(&mut child, "日志跟随")?;
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    })?;

    let status = child.wait().context("无法等待 Docker Compose 完成")?;
    if disconnected {
        return Ok(());
    }
    if !status.success() {
        anyhow::bail!(
            "docker compose logs 执行失败，退出码 {:?}: {}",
            status.code(),
            detail.trim()
        );
    }
    Ok(())
}

/// 使用 Docker Compose 解析并验证项目配置
pub fn compose_validate(app_dir: &Path) -> anyhow::Result<()> {
    run_compose(app_dir, &["config", "--quiet"]).map(|_| ())
}

/// 为指定项目构建参数完整且与当前目录无关的 Compose 命令
fn compose_command(app_dir: &Path) -> anyhow::Result<Command> {
    let compose_file = app_dir.join(COMPOSE_FILE);
    let env_file = app_dir.join(ENV_FILE);
    if !app_dir.is_dir() {
        anyhow::bail!("Compose 项目目录不存在: {}", app_dir.display());
    }
    if !compose_file.is_file() {
        anyhow::bail!("Compose 文件不存在: {}", compose_file.display());
    }
    if !env_file.is_file() {
        anyhow::bail!("环境变量文件不存在: {}", env_file.display());
    }

    let project_name = app_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("无法从目录取得项目名称: {}", app_dir.display()))?;

    let mut command = Command::new("docker");
    command
        .current_dir(app_dir)
        .arg("compose")
        .arg("--project-directory")
        .arg(app_dir)
        .arg("--env-file")
        .arg(&env_file)
        .arg("--file")
        .arg(&compose_file)
        .arg("--project-name")
        .arg(project_name);
    Ok(command)
}

/// 执行 Compose 子命令并返回标准输出
fn run_compose(app_dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let mut command = compose_command(app_dir)?;
    let output = command
        .args(args)
        .output()
        .with_context(|| format!("无法执行 Docker Compose: {}", app_dir.display()))?;
    process::ensure_success(&output, &format!("docker compose {}", args.join(" ")))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// 解析 Docker Compose 的单行 JSON 进度。
fn parse_pull_progress(line: &str) -> anyhow::Result<Option<PullProgress>> {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if value.get("error").and_then(serde_json::Value::as_bool) == Some(true) {
        let message = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Docker Compose 拉取失败");
        anyhow::bail!("{message}");
    }
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let status = value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let text = value
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if id.is_empty() && status.is_empty() && text.is_empty() {
        return Ok(None);
    }
    Ok(Some(PullProgress {
        id: id.to_string(),
        status: status.to_string(),
        text: text.to_string(),
        current: value
            .get("current")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        total: value
            .get("total")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
    }))
}

/// 有界保存 Docker 的非 JSON 错误详情。
fn append_detail(detail: &mut String, line: &str) {
    const MAX_DETAIL_BYTES: usize = 16 * 1024;
    if detail.len() >= MAX_DETAIL_BYTES {
        return;
    }
    let remaining = MAX_DETAIL_BYTES - detail.len();
    let boundary = if line.len() <= remaining {
        line.len()
    } else {
        line.char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= remaining)
            .last()
            .unwrap_or_default()
    };
    detail.push_str(&line[..boundary]);
    detail.push('\n');
}

/// 在调用方断开后终止仍在运行的 Compose 子进程。
fn terminate_child(child: &mut std::process::Child, operation: &str) -> anyhow::Result<()> {
    if child.try_wait()?.is_none() {
        child
            .kill()
            .with_context(|| format!("无法终止已断开连接的{operation}"))?;
    }
    Ok(())
}
