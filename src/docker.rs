//! Docker CLI 与 Docker Compose 子进程封装。

use crate::config::Config;
use crate::constants::{COMPOSE_FILE, ENV_FILE};
use anyhow::Context;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

/// 单个结构化或文本形式的镜像拉取进度事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullProgress {
    /// Docker 进度节点标识。
    pub id: String,
    /// 节点状态。
    pub status: String,
    /// 供用户阅读的详细信息。
    pub text: String,
    /// 可用时表示当前已处理字节数。
    pub current: u64,
    /// 可用时表示总字节数。
    pub total: u64,
}

/// 检查能否连接已配置的 Docker daemon。
#[must_use]
pub fn available(config: &Config) -> bool {
    let mut command = docker_command(config);
    command.args(["info", "--format", "{{json .ServerVersion}}"]);
    command.output().is_ok_and(|output| output.status.success())
}

/// 使用 `docker compose config` 校验暂存的 Compose 项目。
///
/// # 错误
///
/// 校验失败时返回 Docker 诊断信息。
pub fn compose_config(
    config: &Config,
    directory: &Path,
    project_name: &str,
) -> anyhow::Result<String> {
    run_compose_for_project(config, directory, project_name, &["config"])
}

/// 以后台模式启动整个项目或单个服务。
///
/// # 错误
///
/// Docker Compose 执行失败时返回错误。
pub fn compose_up(config: &Config, directory: &Path, service: Option<&str>) -> anyhow::Result<()> {
    let mut args = vec!["up", "-d"];
    if let Some(service) = service {
        args.push(service);
    }
    let _output = run_compose(config, directory, &args)?;
    Ok(())
}

/// 停止项目容器但不删除容器。
///
/// # 错误
///
/// Docker Compose 执行失败时返回错误。
pub fn compose_stop(config: &Config, directory: &Path) -> anyhow::Result<()> {
    let _output = run_compose(config, directory, &["stop", "--timeout", "30"])?;
    Ok(())
}

/// 重启项目容器。
///
/// # 错误
///
/// Docker Compose 执行失败时返回错误。
pub fn compose_restart(config: &Config, directory: &Path) -> anyhow::Result<()> {
    let _output = run_compose(config, directory, &["restart", "--timeout", "30"])?;
    Ok(())
}

/// 停止并删除项目容器和网络。
///
/// # 错误
///
/// Docker Compose 执行失败时返回错误。
pub fn compose_down(config: &Config, directory: &Path) -> anyhow::Result<()> {
    let _output = run_compose(config, directory, &["down", "--timeout", "30"])?;
    Ok(())
}

/// 构建项目声明的镜像。
///
/// # 错误
///
/// Docker Compose 执行失败时返回错误。
pub fn compose_build(config: &Config, directory: &Path) -> anyhow::Result<()> {
    let _output = run_compose(config, directory, &["build"])?;
    Ok(())
}

/// 以逐行 JSON 返回 Compose 容器状态。
///
/// # 错误
///
/// Docker Compose 执行失败时返回错误。
pub fn compose_ps(config: &Config, directory: &Path) -> anyhow::Result<String> {
    run_compose(config, directory, &["ps", "--format", "json"])
}

/// 拉取镜像并报告进度事件。
///
/// 回调返回 `false` 时停止传递事件并终止子进程。
///
/// # 错误
///
/// Docker Compose 执行失败或输出无法读取时返回错误。
pub fn compose_pull(
    config: &Config,
    directory: &Path,
    mut report: impl FnMut(PullProgress) -> bool,
    mut connected: impl FnMut() -> bool,
) -> anyhow::Result<()> {
    let mut command = compose_command(config, directory)?;
    let mut child = command
        .args(["--progress", "json", "pull"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("无法执行 Docker Compose: {}", directory.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 docker compose pull 输出"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 docker compose pull 错误输出"))?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut stopped = false;
    std::thread::scope(|scope| {
        let output_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if output_sender.send(line).is_err() {
                    break;
                }
            }
        });
        let error_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if error_sender.send(line).is_err() {
                    break;
                }
            }
        });
        drop(sender);
        loop {
            if !connected() {
                stopped = true;
                terminate_child(&mut child, "镜像拉取")?;
                break;
            }
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let line = line?;
                    if !report(parse_pull_progress(&line)) {
                        stopped = true;
                        terminate_child(&mut child, "镜像拉取")?;
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        anyhow::Ok(())
    })?;
    let status = child.wait().context("无法等待 docker compose pull")?;
    if stopped {
        return Ok(());
    }
    if !status.success() {
        anyhow::bail!("docker compose pull 失败，退出码 {:?}", status.code());
    }
    Ok(())
}

/// 逐行流式返回 Compose 日志。
///
/// 回调返回 `false` 时停止跟随并终止子进程。
///
/// # 错误
///
/// Docker Compose 执行失败或输出无法读取时返回错误。
pub fn compose_logs(
    config: &Config,
    directory: &Path,
    tail: u32,
    follow: bool,
    mut report: impl FnMut(String) -> bool,
    mut connected: impl FnMut() -> bool,
) -> anyhow::Result<()> {
    let tail = tail.clamp(1, 10_000).to_string();
    let mut command = compose_command(config, directory)?;
    command.args(["logs", "--no-color", "--tail", &tail]);
    if follow {
        command.arg("--follow");
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("无法执行 Docker Compose: {}", directory.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 docker compose logs 输出"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取 docker compose logs 错误输出"))?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut stopped = false;
    std::thread::scope(|scope| {
        let output_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if output_sender.send(line).is_err() {
                    break;
                }
            }
        });
        let error_sender = sender.clone();
        scope.spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if error_sender.send(line).is_err() {
                    break;
                }
            }
        });
        drop(sender);
        loop {
            if !connected() {
                stopped = true;
                terminate_child(&mut child, "日志跟随")?;
                break;
            }
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    if !report(line?) {
                        stopped = true;
                        terminate_child(&mut child, "日志跟随")?;
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        anyhow::Ok(())
    })?;
    let status = child.wait().context("无法等待 docker compose logs")?;
    if stopped || status.success() {
        return Ok(());
    }
    anyhow::bail!("docker compose logs 失败，退出码 {:?}", status.code());
}

/// 在客户端断开后终止仍在运行的 Compose 子进程。
fn terminate_child(child: &mut std::process::Child, operation: &str) -> anyhow::Result<()> {
    if child.try_wait()?.is_none() {
        child
            .kill()
            .with_context(|| format!("无法终止已断开连接的{operation}进程"))?;
    }
    Ok(())
}

/// 创建固定使用已配置 socket 的 Docker 命令。
fn docker_command(config: &Config) -> Command {
    let mut command = Command::new("docker");
    command.env(
        "DOCKER_HOST",
        format!("unix://{}", config.docker_socket.display()),
    );
    command
}

/// 为单个受管目录创建参数完整的 Compose 命令。
fn compose_command(config: &Config, directory: &Path) -> anyhow::Result<Command> {
    let project = directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("项目目录名无效: {}", directory.display()))?;
    compose_command_for_project(config, directory, project)
}

/// 为暂存目录创建使用真实受管项目名的 Compose 命令。
fn compose_command_for_project(
    config: &Config,
    directory: &Path,
    project_name: &str,
) -> anyhow::Result<Command> {
    let compose = directory.join(COMPOSE_FILE);
    let env = directory.join(ENV_FILE);
    if !compose.is_file() || !env.is_file() {
        anyhow::bail!("项目状态不完整: {}", directory.display());
    }
    let mut command = docker_command(config);
    command
        .current_dir(directory)
        .arg("compose")
        .arg("--project-directory")
        .arg(directory)
        .arg("--env-file")
        .arg(env)
        .arg("--file")
        .arg(compose)
        .arg("--project-name")
        .arg(project_name);
    Ok(command)
}

/// 执行 Compose 子命令并返回以有损 UTF-8 解码的标准输出。
fn run_compose(config: &Config, directory: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = compose_command(config, directory)?
        .args(args)
        .output()
        .with_context(|| format!("无法执行 Docker Compose: {}", directory.display()))?;
    ensure_success(&output, &format!("docker compose {}", args.join(" ")))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 使用显式项目名执行暂存目录中的 Compose 子命令。
fn run_compose_for_project(
    config: &Config,
    directory: &Path,
    project_name: &str,
    args: &[&str],
) -> anyhow::Result<String> {
    let output = compose_command_for_project(config, directory, project_name)?
        .args(args)
        .output()
        .with_context(|| format!("无法执行 Docker Compose: {}", directory.display()))?;
    ensure_success(&output, &format!("docker compose {}", args.join(" ")))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 将失败进程的输出转换为有界诊断信息。
fn ensure_success(output: &Output, operation: &str) -> anyhow::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::bail!(
        "{operation} 失败，退出码 {:?}: {}{}",
        output.status.code(),
        stderr.trim(),
        stdout.trim()
    );
}

/// 解析 Docker JSON 进度，并将非结构化行保留为文本。
fn parse_pull_progress(line: &str) -> PullProgress {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return PullProgress {
            id: String::new(),
            status: String::new(),
            text: line.to_string(),
            current: 0,
            total: 0,
        };
    };
    PullProgress {
        id: value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        status: value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        text: value
            .get("text")
            .or_else(|| value.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(line)
            .to_string(),
        current: value
            .get("current")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        total: value
            .get("total")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::compose_command_for_project;
    use crate::config::Config;

    /// 隐藏暂存目录必须使用真实项目名，不能把非法目录名交给 Compose。
    #[test]
    fn staged_compose_uses_managed_project_name() -> anyhow::Result<()> {
        let directory = std::env::temp_dir().join(format!(
            ".traefik.stage-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir(&directory)?;
        std::fs::write(
            directory.join(crate::constants::COMPOSE_FILE),
            "services:\n  traefik:\n    image: traefik:v3.8.0\n",
        )?;
        std::fs::write(directory.join(crate::constants::ENV_FILE), "")?;

        let result = (|| -> anyhow::Result<()> {
            let command = compose_command_for_project(&Config::default(), &directory, "traefik")?;
            let args: Vec<String> = command
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect();
            let index = args
                .iter()
                .position(|value| value == "--project-name")
                .ok_or_else(|| anyhow::anyhow!("Compose 命令缺少 --project-name"))?;
            assert_eq!(args.get(index + 1).map(String::as_str), Some("traefik"));
            Ok(())
        })();
        let cleanup = std::fs::remove_dir_all(&directory);
        result?;
        cleanup?;
        Ok(())
    }
}
