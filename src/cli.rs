//! 命令行参数与子命令定义。

use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub mod generated;
pub use generated::{AppCmd, InfraCmd};

/// Nihility 机器守护进程管理工具。
///
/// 通过 gRPC 调用 root systemd 服务，管理 Linux 系统目录中的 Compose 项目。
#[derive(Parser)]
#[command(name = "nsetup", version, about)]
pub struct Cli {
    #[command(subcommand)]
    /// 子命令。
    pub command: Commands,
}

/// 支持的 CLI 子命令。
#[derive(Subcommand)]
pub enum Commands {
    /// 从当前可执行文件初始化系统服务。
    Init {
        /// 覆盖已有的单文件安装、配置和 unit。
        #[arg(long)]
        force: bool,
    },
    /// 启动 gRPC 编排守护进程。
    #[command(hide = true)]
    Daemon,
    /// 管理已安装的 systemd 服务。
    Service {
        #[command(subcommand)]
        /// 系统服务操作。
        action: ServiceCmd,
    },
    /// 管理 Traefik 基础设施。
    Infra {
        #[command(subcommand)]
        /// 基础设施操作。
        action: InfraCmd,
    },
    /// 生成受 Nihility 管理的应用。
    App {
        #[command(subcommand)]
        /// 应用生成操作。
        action: AppCmd,
    },
    /// 调用正在运行的 gRPC 守护进程。
    Rpc {
        /// 服务端点；默认使用本机配置。
        #[arg(long)]
        endpoint: Option<String>,
        /// TCP 认证令牌文件；Unix socket 模式不使用。
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[command(subcommand)]
        /// 远程操作。
        action: RpcCmd,
    },
    /// 从现有 Compose 和 env 文件创建或更新项目。
    Deploy {
        /// 项目名称。
        name: String,
        /// Compose YAML 文件路径。
        #[arg(long)]
        compose: PathBuf,
        /// 环境变量文件路径；不指定时创建空文件。
        #[arg(long)]
        env_file: Option<PathBuf>,
        /// 部署完成后立即启动。
        #[arg(long)]
        start: bool,
    },
    /// 查看项目最近日志。
    Logs {
        /// 项目名称。
        app: String,
        /// 每个服务保留的日志行数。
        #[arg(long, default_value_t = 200)]
        tail: u32,
        /// 持续输出新增日志。
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// 列出所有 Compose 项目及其状态。
    List,
    /// 启动 Compose 项目。
    Start {
        /// 应用名称。
        app: String,
    },
    /// 停止 Compose 项目。
    Stop {
        /// 应用名称。
        app: String,
    },
    /// 重启 Compose 项目。
    Restart {
        /// 应用名称。
        app: String,
    },
    /// 设置应用镜像版本，拉取镜像并重新创建服务。
    Upgrade {
        /// 应用名称。
        app: String,
        /// Compose 服务名；单服务应用可以省略。
        #[arg(long)]
        service: Option<String>,
        /// 新镜像版本标签。
        #[arg(long, value_parser = validate_version_arg)]
        version: String,
    },
    /// 拉取 Compose 项目的最新镜像。
    Pull {
        /// 应用名称。
        app: String,
    },
    /// 构建 Compose 项目镜像。
    Build {
        /// 应用名称。
        app: String,
    },
    /// 查看 Compose 项目详细信息。
    Show {
        /// 应用名称。
        app: String,
    },
    /// 移除应用。
    Remove {
        /// 应用名称。
        app: String,
        /// 跳过确认提示。
        #[arg(long)]
        force: bool,
        /// 同时删除 Compose 命名卷，不删除外部绑定数据。
        #[arg(long)]
        purge: bool,
    },
    /// 检查本机编排运行环境状态。
    Status,
}

/// systemd 系统服务操作。
#[derive(Debug, Clone, Copy, Subcommand)]
pub enum ServiceCmd {
    /// 启动服务。
    Start,
    /// 停止服务。
    Stop,
    /// 重启服务。
    Restart,
    /// 查看服务状态。
    Status,
}

/// gRPC 远程操作。
#[derive(Subcommand)]
pub enum RpcCmd {
    /// 查询守护进程和 Docker 健康状态。
    Health,
    /// 列出远程项目。
    List,
    /// 创建或更新远程项目。
    Deploy {
        /// 项目名称。
        name: String,
        /// Compose YAML 文件路径。
        #[arg(long)]
        compose: PathBuf,
        /// 环境变量文件路径。
        #[arg(long)]
        env_file: Option<PathBuf>,
        /// 部署完成后立即启动。
        #[arg(long)]
        start: bool,
    },
    /// 删除远程项目。
    Remove {
        /// 项目名称。
        name: String,
        /// 同时删除 Compose 命名卷。
        #[arg(long)]
        volumes: bool,
    },
    /// 启动远程项目。
    Start {
        /// 项目名称。
        name: String,
    },
    /// 停止远程项目。
    Stop {
        /// 项目名称。
        name: String,
    },
    /// 重启远程项目。
    Restart {
        /// 项目名称。
        name: String,
    },
    /// 设置服务镜像版本并立即更新。
    Upgrade {
        /// 项目名称。
        name: String,
        /// Compose 服务名；单服务应用可以省略。
        #[arg(long)]
        service: Option<String>,
        /// 新镜像版本标签。
        #[arg(long, value_parser = validate_version_arg)]
        version: String,
    },
    /// 拉取远程项目镜像。
    Pull {
        /// 项目名称。
        name: String,
    },
    /// 构建远程项目镜像。
    Build {
        /// 项目名称。
        name: String,
    },
    /// 查看远程项目详情。
    Show {
        /// 项目名称。
        name: String,
    },
    /// 查看远程项目日志。
    Logs {
        /// 项目名称。
        name: String,
        /// 每个服务保留的日志行数。
        #[arg(long, default_value_t = 200)]
        tail: u32,
        /// 持续输出新增日志。
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

/// 在发起 RPC 前校验镜像版本参数。
fn validate_version_arg(value: &str) -> Result<String, String> {
    if crate::orchestrator::valid_image_version(value) {
        Ok(value.to_string())
    } else {
        Err(String::from(
            "必须指定明确的镜像版本标签，且不能使用 latest",
        ))
    }
}
