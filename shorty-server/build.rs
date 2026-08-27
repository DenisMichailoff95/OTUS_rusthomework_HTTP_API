use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("proto");

    let proto_file = proto_dir.join("shorty/v1/shorty.proto");
    let fd_path = proto_dir.join("shorty.v1.shorty.bin");

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(&fd_path)
        .compile_protos(&[proto_file], &[proto_dir])?;

    Ok(())
}
