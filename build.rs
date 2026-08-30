//! 编译并完善 Nihility gRPC 协议生成代码。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/nsetup.proto");
    tonic_prost_build::configure().compile_protos(&["proto/nsetup.proto"], &["proto"])?;
    Ok(())
}
