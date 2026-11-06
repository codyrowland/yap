//! Downloading, verifying and unpacking the files the catalogs list
//! (`models`, `cleanup`). Every download is checked against a pinned size
//! and SHA-256 before use.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Written last into an unpacked folder; its presence means the unpack
/// finished.
pub const COMPLETE_MARKER: &str = ".yap-complete";

pub fn unpacked(folder: &Path) -> bool {
    folder.join(COMPLETE_MARKER).is_file()
}

pub fn verify(file: &mut File, expected: &str) -> Result<(), String> {
    let mut hasher = Sha256::new();
    std::io::copy(file, &mut hasher).map_err(|error| error.to_string())?;
    if format!("{:x}", hasher.finalize()) != expected {
        return Err("Downloaded model failed integrity check".to_owned());
    }

    Ok(())
}

/// Downloads `url` to `partial`, checking the exact size and SHA-256.
/// `progress` gets the bytes downloaded so far. Leaves the file in place on
/// success; the caller moves or unpacks it.
pub fn fetch(
    url: &str,
    partial: &Path,
    bytes: u64,
    sha256: &str,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(3_600))
        .build()
        .map_err(|_| "Could not start model download".to_owned())?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|_| "Could not connect to model host".to_owned())?
        .error_for_status()
        .map_err(|_| "Model download failed".to_owned())?;
    if response
        .content_length()
        .is_some_and(|length| length != bytes)
    {
        return Err("Model download had unexpected size".to_owned());
    }

    let mut file = File::create(partial).map_err(|error| error.to_string())?;
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
        if downloaded > bytes {
            return Err("Model download exceeded expected size".to_owned());
        }

        file.write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        progress(downloaded);
    }

    if downloaded != bytes {
        return Err("Incomplete model download".to_owned());
    }

    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    let mut file = File::open(partial).map_err(|error| error.to_string())?;
    verify(&mut file, sha256)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("Cancelled".to_owned());
    }
    Ok(())
}

/// `parakeet-tdt-0.6b-v3` → `parakeet-tdt-0.6b-v3.unpacking` (model ids
/// contain dots, so `with_extension` would mangle them).
pub fn staging_dir(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".unpacking");
    destination.with_file_name(name)
}

/// Fills a staging folder with `fill`, marks it complete and moves it to
/// `destination`. On failure nothing is left behind.
fn into_folder(
    destination: &Path,
    fill: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let staging = staging_dir(destination);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let result = fill(&staging).and_then(|()| {
        fs::write(staging.join(COMPLETE_MARKER), []).map_err(|error| error.to_string())?;
        let _ = fs::remove_dir_all(destination);
        fs::rename(&staging, destination).map_err(|error| error.to_string())
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Unpacks the files named in `wanted` from a verified `.tar.bz2` into
/// `destination`. Entries are matched by file name only and written under
/// fixed names, so archive paths can never escape the folder.
pub fn unpack_tar_bz2(
    archive: &Path,
    destination: &Path,
    wanted: &[&str],
    cancelled: &AtomicBool,
) -> Result<(), String> {
    into_folder(destination, |staging| {
        let file = File::open(archive).map_err(|error| error.to_string())?;
        let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(std::io::BufReader::new(file)));
        let mut found = vec![false; wanted.len()];
        for entry in tar
            .entries()
            .map_err(|_| "Model archive is damaged".to_owned())?
        {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled".to_owned());
            }
            let mut entry = entry.map_err(|_| "Model archive is damaged".to_owned())?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let name = entry.path().ok().and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            });
            let Some(index) = name
                .as_deref()
                .and_then(|name| wanted.iter().position(|file| *file == name))
            else {
                continue;
            };
            let mut output =
                File::create(staging.join(wanted[index])).map_err(|error| error.to_string())?;
            std::io::copy(&mut entry, &mut output)
                .map_err(|_| "Could not unpack model".to_owned())?;
            found[index] = true;
        }
        if let Some(missing) = wanted
            .iter()
            .zip(found)
            .find_map(|(name, found)| (!found).then_some(*name))
        {
            return Err(format!("Model archive is missing {missing}"));
        }
        Ok(())
    })
}

/// Unpacks the files `keep` accepts from a verified zip into `destination`;
/// `required` must be among them. Names are taken from the final path
/// component only, so entries cannot escape the folder.
pub fn unpack_zip(
    archive: &Path,
    destination: &Path,
    keep: impl Fn(&str) -> bool,
    required: &str,
) -> Result<(), String> {
    into_folder(destination, |staging| {
        let file = File::open(archive).map_err(|error| error.to_string())?;
        let mut zip =
            zip::ZipArchive::new(file).map_err(|_| "Runtime archive is damaged".to_owned())?;
        for index in 0..zip.len() {
            let mut entry = zip
                .by_index(index)
                .map_err(|_| "Runtime archive is damaged".to_owned())?;
            if !entry.is_file() {
                continue;
            }
            let Some(name) = Path::new(entry.name())
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if !keep(&name) {
                continue;
            }
            let mut output =
                File::create(staging.join(&name)).map_err(|error| error.to_string())?;
            std::io::copy(&mut entry, &mut output)
                .map_err(|_| "Could not unpack the clean-up runtime".to_owned())?;
        }
        if !staging.join(required).is_file() {
            return Err(format!("Runtime archive is missing {required}"));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

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
        assert!(
            verify(
                &mut file,
                "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21"
            )
            .is_err()
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn staging_folder_keeps_dotted_names() {
        assert_eq!(
            staging_dir(Path::new("models/parakeet-tdt-0.6b-v3"))
                .file_name()
                .unwrap(),
            "parakeet-tdt-0.6b-v3.unpacking"
        );
    }
}
