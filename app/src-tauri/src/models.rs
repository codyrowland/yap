//! Downloadable on-device models.
//!
//! - Whisper (whisper.cpp): one `.bin` file per size, from Hugging Face.
//! - Parakeet, Moonshine and Nemotron (sherpa-onnx): a `.tar.bz2` per size
//!   from the sherpa-onnx `asr-models` GitHub release, unpacked into a folder
//!   named after the model id. Only the files the recognizer needs are
//!   extracted (see [`required_files`]).
//!
//! Downloading itself is in [`crate::download`].

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::download;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Whisper,
    /// NVIDIA Parakeet TDT, offline transducer.
    Parakeet,
    /// Useful Sensors Moonshine v2, offline encoder + merged decoder.
    Moonshine,
    /// NVIDIA Nemotron streaming transducer: decodes while you speak.
    Nemotron,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Whisper => "Whisper",
            Family::Parakeet => "Parakeet",
            Family::Moonshine => "Moonshine",
            Family::Nemotron => "Nemotron",
        }
    }

    /// Whisper ships as one file; the sherpa-onnx families as archives.
    pub fn archived(self) -> bool {
        self != Family::Whisper
    }

    /// Nemotron models stream: they produce text while recording.
    pub fn streaming(self) -> bool {
        self == Family::Nemotron
    }
}

pub struct Model {
    pub id: &'static str,
    pub family: Family,
    /// Name shown in the UI after the family name.
    pub label: &'static str,
    pub note: &'static str,
    /// The downloaded file: the model itself (Whisper) or an archive (Parakeet).
    pub filename: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

const TRANSDUCER_FILES: &[&str] = &[
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
];
const MOONSHINE_FILES: &[&str] = &[
    "encoder_model.ort",
    "decoder_model_merged.ort",
    "tokens.txt",
];

/// Files a model folder must contain, found by name inside the archive.
pub fn required_files(family: Family) -> &'static [&'static str] {
    match family {
        Family::Whisper => &[],
        Family::Parakeet | Family::Nemotron => TRANSDUCER_FILES,
        Family::Moonshine => MOONSHINE_FILES,
    }
}

pub const MODELS: [Model; 12] = [
    Model {
        id: "tiny",
        family: Family::Whisper,
        label: "tiny",
        note: "Fastest · lowest accuracy",
        filename: "ggml-tiny.bin",
        bytes: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    Model {
        id: "base",
        family: Family::Whisper,
        label: "base",
        note: "Fast · short notes",
        filename: "ggml-base.bin",
        bytes: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    Model {
        id: "small",
        family: Family::Whisper,
        label: "small",
        note: "Balanced",
        filename: "ggml-small.bin",
        bytes: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    Model {
        id: "medium",
        family: Family::Whisper,
        label: "medium",
        note: "Accurate · slower",
        filename: "ggml-medium.bin",
        bytes: 1_533_763_059,
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    },
    Model {
        id: "large-v3",
        family: Family::Whisper,
        label: "large-v3",
        note: "Most accurate · slowest",
        filename: "ggml-large-v3.bin",
        bytes: 3_095_033_483,
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    },
    Model {
        id: "large-v3-turbo",
        family: Family::Whisper,
        label: "large-v3-turbo",
        note: "Near large-v3 accuracy · much faster",
        filename: "ggml-large-v3-turbo.bin",
        bytes: 1_624_555_275,
        sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    },
    Model {
        id: "parakeet-tdt-0.6b-v3",
        family: Family::Parakeet,
        label: "TDT 0.6B v3",
        note: "25 European languages · detects language · fast on CPU",
        filename: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
        bytes: 487_211_443,
        sha256: "1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9",
    },
    Model {
        id: "parakeet-tdt-0.6b-v2",
        family: Family::Parakeet,
        label: "TDT 0.6B v2",
        note: "English only · very low latency",
        filename: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2",
        bytes: 482_440_461,
        sha256: "6fc4426b54aa549a0019b8a75b8dea59164e71790a37ac56a045ad57f915f714",
    },
    Model {
        id: "moonshine-tiny-en",
        family: Family::Moonshine,
        label: "tiny",
        note: "English · smallest · runs on any laptop",
        filename: "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27.tar.bz2",
        bytes: 29_858_559,
        sha256: "9ec31b342d8fa3240c3b81b8f82e1cf7e3ac467c93ca5a999b741d5887164f8d",
    },
    Model {
        id: "moonshine-base-en",
        family: Family::Moonshine,
        label: "base",
        note: "English · better accuracy, still light",
        filename: "sherpa-onnx-moonshine-base-en-quantized-2026-02-27.tar.bz2",
        bytes: 111_266_225,
        sha256: "43232c1d13013d37317163baec3135bd771a186a4356f28c889bab453bb0e891",
    },
    Model {
        id: "nemotron-streaming-en-0.6b",
        family: Family::Nemotron,
        label: "Speech Streaming EN 0.6B",
        note: "English · text appears while you speak",
        filename: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25.tar.bz2",
        bytes: 463_945_051,
        sha256: "78e2b79fcf7271553a74402a76b771b09ea40117a39566a79f52235b23db6358",
    },
    Model {
        id: "nemotron-3.5-streaming-0.6b",
        family: Family::Nemotron,
        label: "3.5 ASR Streaming 0.6B",
        note: "Multilingual · uses the spoken-language setting · live text",
        filename: "sherpa-onnx-nemotron-3.5-asr-streaming-0.6b-560ms-int8-2026-06-11.tar.bz2",
        bytes: 475_271_763,
        sha256: "c6bf5e0df765f9d5b43bc9e0536d4b4b3e7d40bdf5ecf13e45f134c51c05ae3a",
    },
];

pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|model| model.id == id)
}

/// The model a saved `local_model_path` points at, matched by its final path
/// component (a Whisper file name or a sherpa-onnx model folder name).
pub fn for_path(path: &Path) -> Option<&'static Model> {
    let name = path.file_name()?.to_str()?;
    MODELS.iter().find(|model| {
        if model.family.archived() {
            model.id == name
        } else {
            model.filename == name
        }
    })
}

/// Whether the spoken-language setting applies. Parakeet detects the
/// language itself; Moonshine and Nemotron EN are single-language.
pub fn accepts_language(model: &Model) -> bool {
    model.family == Family::Whisper || model.id == "nemotron-3.5-streaming-0.6b"
}

/// Whether a saved `local_model_path` points at a complete download: a
/// Whisper file, or a sherpa-onnx model folder with all its files. Paths that
/// are not in the catalog (a custom whisper.cpp file) only need to exist.
pub fn path_installed(path: &Path) -> bool {
    match (for_path(path), path.parent()) {
        (Some(model), Some(directory)) => installed(model, directory),
        _ => path.is_file(),
    }
}

/// "Whisper large-v3-turbo", "Parakeet TDT 0.6B v3", "Moonshine tiny".
pub fn display_name(model: &Model) -> String {
    format!("{} {}", model.family.name(), model.label)
}

/// Where an installed model lives: a file for Whisper, a folder otherwise.
pub fn path(model: &Model, directory: &Path) -> PathBuf {
    if model.family.archived() {
        directory.join(model.id)
    } else {
        directory.join(model.filename)
    }
}

fn url(model: &Model) -> String {
    if model.family.archived() {
        format!(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/{}",
            model.filename
        )
    } else {
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
            model.filename
        )
    }
}

pub fn installed(model: &Model, directory: &Path) -> bool {
    let path = path(model, directory);
    if model.family.archived() {
        download::unpacked(&path)
            && required_files(model.family)
                .iter()
                .all(|file| path.join(file).is_file())
    } else {
        fs::metadata(&path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == model.bytes)
    }
}

/// Deletes a downloaded model. Missing files are not an error.
pub fn remove(model: &Model, directory: &Path) -> Result<(), String> {
    let path = path(model, directory);
    let result = if model.family.archived() {
        fs::remove_dir_all(&path)
    } else {
        fs::remove_file(&path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Downloads, verifies and (for Parakeet) unpacks a model. `progress` gets
/// the number of bytes downloaded so far.
pub fn download(
    model: &Model,
    directory: &Path,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<PathBuf, String> {
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let final_path = path(model, directory);
    if !model.family.archived() && installed(model, directory) {
        let mut file = File::open(&final_path).map_err(|error| error.to_string())?;
        if download::verify(&mut file, model.sha256).is_ok() {
            return Ok(final_path);
        }
    }
    if model.family.archived() && installed(model, directory) {
        return Ok(final_path);
    }

    let partial = directory.join(format!("{}.part", model.filename));
    let result = (|| {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Cancelled".to_owned());
        }

        download::fetch(
            &url(model),
            &partial,
            model.bytes,
            model.sha256,
            cancelled,
            &mut progress,
        )?;
        if model.family.archived() {
            download::unpack_tar_bz2(
                &partial,
                &final_path,
                required_files(model.family),
                cancelled,
            )?;
        } else {
            fs::rename(&partial, &final_path).map_err(|error| error.to_string())?;
        }
        Ok(final_path.clone())
    })();
    // The archive is only needed until it is unpacked.
    if result.is_err() || model.family.archived() {
        let _ = fs::remove_file(partial);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{COMPLETE_MARKER, staging_dir, unpack_tar_bz2};
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("yap-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn model_ids_only_resolve_to_known_files() {
        assert_eq!(model("base").unwrap().filename, "ggml-base.bin");
        assert!(model("../../secret").is_none());
        let ids = MODELS
            .iter()
            .map(|model| model.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), MODELS.len());
    }

    #[test]
    fn saved_paths_map_back_to_models() {
        let directory = Path::new(r"C:\Users\Test\models");
        for model in &MODELS {
            assert_eq!(for_path(&path(model, directory)).unwrap().id, model.id);
        }
        assert!(for_path(Path::new(r"C:\other\custom.bin")).is_none());
        assert_eq!(
            display_name(model("parakeet-tdt-0.6b-v3").unwrap()),
            "Parakeet TDT 0.6B v3"
        );
    }

    #[test]
    fn folder_models_count_as_installed() {
        let directory = temp_dir("path-installed");
        let model = model("parakeet-tdt-0.6b-v3").unwrap();
        let folder = path(model, &directory);
        assert!(!path_installed(&folder));
        fs::create_dir_all(&folder).unwrap();
        for file in required_files(model.family) {
            fs::write(folder.join(file), b"x").unwrap();
        }
        assert!(!path_installed(&folder), "unfinished unpack must not count");
        fs::write(folder.join(COMPLETE_MARKER), []).unwrap();
        assert!(path_installed(&folder));
        let custom = directory.join("my-finetune.bin");
        assert!(!path_installed(&custom));
        fs::write(&custom, b"x").unwrap();
        assert!(path_installed(&custom));
        fs::remove_dir_all(directory).unwrap();
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut compressed = Vec::new();
        let mut encoder = bzip2::write::BzEncoder::new(&mut compressed, bzip2::Compression::fast());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap();
        compressed
    }

    #[test]
    fn unpacks_only_the_needed_parakeet_files() {
        let directory = temp_dir("unpack");
        let source = directory.join("model.tar.bz2");
        fs::write(
            &source,
            archive(&[
                ("sherpa-onnx-nemo-parakeet/encoder.int8.onnx", b"e"),
                ("sherpa-onnx-nemo-parakeet/decoder.int8.onnx", b"d"),
                ("sherpa-onnx-nemo-parakeet/joiner.int8.onnx", b"j"),
                ("sherpa-onnx-nemo-parakeet/tokens.txt", b"t"),
                ("sherpa-onnx-nemo-parakeet/test_wavs/0.wav", b"w"),
            ]),
        )
        .unwrap();
        let destination = directory.join("parakeet-tdt-0.6b-v3");
        unpack_tar_bz2(
            &source,
            &destination,
            TRANSDUCER_FILES,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(installed(
            model("parakeet-tdt-0.6b-v3").unwrap(),
            &directory
        ));
        assert_eq!(fs::read(destination.join("tokens.txt")).unwrap(), b"t");
        assert!(!destination.join("0.wav").exists());
        assert!(!destination.join("test_wavs").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unpacks_moonshine_v2_files() {
        let directory = temp_dir("moonshine");
        let source = directory.join("model.tar.bz2");
        fs::write(
            &source,
            archive(&[
                ("m/encoder_model.ort", b"e"),
                ("m/decoder_model_merged.ort", b"d"),
                ("m/tokens.txt", b"t"),
                ("m/LICENSE", b"l"),
            ]),
        )
        .unwrap();
        let model = model("moonshine-tiny-en").unwrap();
        unpack_tar_bz2(
            &source,
            &path(model, &directory),
            required_files(model.family),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(installed(model, &directory));
        assert!(!path(model, &directory).join("LICENSE").exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn every_family_has_files_and_only_nemotron_streams() {
        for model in &MODELS {
            assert_eq!(
                model.family.archived(),
                !required_files(model.family).is_empty()
            );
            assert_eq!(model.family.streaming(), model.family == Family::Nemotron);
            assert_eq!(model.sha256.len(), 64, "{}", model.id);
        }
    }

    #[test]
    fn incomplete_parakeet_archive_leaves_nothing_installed() {
        let directory = temp_dir("incomplete");
        let source = directory.join("model.tar.bz2");
        fs::write(&source, archive(&[("x/encoder.int8.onnx", b"e")])).unwrap();
        let destination = directory.join("parakeet-tdt-0.6b-v3");
        let error = unpack_tar_bz2(
            &source,
            &destination,
            TRANSDUCER_FILES,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("missing"), "{error}");
        assert!(!destination.exists());
        assert!(!staging_dir(&destination).exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
