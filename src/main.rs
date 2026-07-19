//! Nihility 项目的机器守护进程与 `nsetup` 管理 CLI。
//!
//! 提供 Docker Compose 项目管理 CLI、gRPC 接口和 systemd 服务控制能力。

/// 命令行参数定义。
mod cli;
/// CLI 命令执行。
mod cli_runner;
/// 配置加载与持久化。
mod config;
/// 常量定义。
mod constants;
/// 基础设施与应用配置生成。
mod generator;
/// 单文件系统初始化。
mod installer;
/// Compose 项目编排逻辑。
mod orchestrator;
/// gRPC 服务。
mod rpc;
/// 核心服务逻辑。
mod services;
/// systemd 服务安装与管理。
mod system_service;

use clap::Parser;
use cli::Cli;
use tracing::error;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_target(false)
        .init();

    if let Err(error) = cli_runner::run(Cli::parse()).await {
        error!("❌ 错误: {}", error);
        let mut source = error.source();
        while let Some(cause) = source {
            error!("   原因: {}", cause);
            source = std::error::Error::source(cause);
        }
        std::process::exit(1);
    }
}
