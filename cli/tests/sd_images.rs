use std::{fs, process::Command};

#[test]
fn sd_attachment_refuses_empty_media_and_unsupported_chips() {
    let path = std::env::temp_dir().join(format!("esp32sim-empty-sd-{}", std::process::id()));
    fs::write(&path, []).unwrap();
    for chip in ["s3", "c3"] {
        let result = Command::new(env!("CARGO_BIN_EXE_esp32sim"))
            .args(["--chip", chip, "--sd-image"]).arg(&path).output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&result.stderr).contains("nonempty regular file"));
    }
    let result = Command::new(env!("CARGO_BIN_EXE_esp32sim"))
        .args(["--chip", "c6", "--sd-image"]).arg(&path).output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("S3 SDMMC or C3 X3 SPI"));
    fs::remove_file(path).unwrap();
}

#[test]
fn explicit_save_never_overwrites_an_existing_file() {
    let path = std::env::temp_dir().join(format!("esp32sim-save-sd-{}", std::process::id()));
    fs::write(&path, b"preserve").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_esp32sim"))
        .args(["--chip", "c3", "--sd-image"]).arg(&path).arg("--sd-save").arg(&path).output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("fresh output path"));
    assert_eq!(fs::read(&path).unwrap(), b"preserve");
    fs::remove_file(path).unwrap();
}
