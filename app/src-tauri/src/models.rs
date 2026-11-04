use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

pub struct Model {
    pub id: &'static str,
    pub filename: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

pub const WHISPER_MODELS: [Model; 6] = [
    Model {
        id: "tiny",
        filename: "ggml-tiny.bin",
        bytes: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    Model {
        id: "base",
        filename: "ggml-base.bin",
        bytes: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    Model {
        id: "small",
        filename: "ggml-small.bin",
        bytes: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    Model {
        id: "medium",
        filename: "ggml-medium.bin",
        bytes: 1_533_763_059,
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    },
    Model {
        id: "large-v3",
        filename: "ggml-large-v3.bin",
        bytes: 3_095_033_483,
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    },
    Model {
        id: "large-v3-turbo",
        filename: "ggml-large-v3-turbo.bin",
        bytes: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    },
];

pub fn model(id: &str) -> Option<&'static Model> {
    WHISPER_MODELS.iter().find(|model| model.id == id)
}

pub fn installed(model: &Model, directory: &Path) -> bool {
    fs::metadata(directory.join(model.filename))
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == model.bytes)
}

/// Deletes a downloaded model file. Missing files are not an error.
pub fn remove(model: &Model, directory: &Path) -> Result<(), String> {
    match fs::remove_file(directory.join(model.filename)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn verify(file: &mut File, expected: &str) -> Result<(), String> {
    let mut hasher = Sha256::new();
    std::io::copy(file, &mut hasher).map_err(|error| error.to_string())?;
    if format!("{:x}", hasher.finalize()) != expected {
        return Err("Downloaded model failed integrity check".to_owned());
    }

    Ok(())
}

pub fn download(
    model: &Model,
    directory: &Path,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<PathBuf, String> {
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let final_path = directory.join(model.filename);
    if installed(model, directory) {
        let mut file = File::open(&final_path).map_err(|error| error.to_string())?;
        if verify(&mut file, model.sha256).is_ok() {
            return Ok(final_path);
        }
    }

    let partial = directory.join(format!("{}.part", model.filename));
    let result = (|| {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(3_600))
            .build()
            .map_err(|_| "Could not start model download".to_owned())?;
        let url = format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
            model.filename
        );
        let mut response = client
            .get(url)
            .send()
            .map_err(|_| "Could not connect to model host".to_owned())?
            .error_for_status()
            .map_err(|_| "Model download failed".to_owned())?;
        if response
            .content_length()
            .is_some_and(|length| length != model.bytes)
        {
            return Err("Model download had unexpected size".to_owned());
        }

        let mut file = File::create(&partial).map_err(|error| error.to_string())?;
        let mut buffer = [0_u8; 64 * 1024];
        let mut downloaded = 0_u64;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled".to_owned());
            }

            let count = response
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }

            downloaded += count as u64;
            if downloaded > model.bytes {
                return Err("Model download exceeded expected size".to_owned());
            }

            file.write_all(&buffer[..count])
                .map_err(|error| error.to_string())?;
            progress(downloaded);
        }

        if downloaded != model.bytes {
            return Err("Incomplete model download".to_owned());
        }

        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        let mut file = File::open(&partial).map_err(|error| error.to_string())?;
        verify(&mut file, model.sha256)?;
        drop(file);
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        fs::rename(&partial, &final_path).map_err(|error| error.to_string())?;
        Ok(final_path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(partial);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

    #[test]
    fn model_ids_only_resolve_to_known_files() {
        assert_eq!(model("base").unwrap().filename, "ggml-base.bin");
        assert!(model("../../secret").is_none());
    }

    #[test]
    fn verifies_content_not_just_file_size() {
        let path = std::env::temp_dir().join(format!("yap-hash-test-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        let mut file = File::open(&path).unwrap();
        assert!(
            verify(
                &mut file,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            )
            .is_ok()
        );
        file.rewind().unwrap();
        assert!(verify(&mut file, WHISPER_MODELS[0].sha256).is_err());
        fs::remove_file(path).unwrap();
    }
}
