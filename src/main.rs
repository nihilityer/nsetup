//! `nsetup` 单二进制 CLI 与特权 daemon。

mod cli;
mod config;
mod constants;
mod docker;
mod doctor;
mod import;
mod install;
mod orchestrator;
mod rpc;
mod spec;
mod template;

/// 解析命令行并运行选定的二进制角色。
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    cli::run(cli::Cli::parse_localized()).await
}
