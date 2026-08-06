fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = "../../proto/mcc/agent/v1/agent.proto";
    let includes = ["../../proto"];
    println!("cargo:rerun-if-changed={proto}");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto], &includes)?;
    Ok(())
}
