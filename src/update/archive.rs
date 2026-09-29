use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Result, paths};

pub(super) const RECEIPT: &str = "gitwatch-install.json";
pub(super) const PROGRAM: &str = if cfg!(windows) {
    "gitwatch.exe"
} else {
    "gitwatch"
};
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub(super) struct Receipt {
    pub version: String,
    pub target: String,
    pub files: BTreeMap<String, String>,
}

impl Receipt {
    pub fn read(directory: &Path) -> Result<Self> {
        let value: Self = serde_json::from_slice(&paths::read_file(&directory.join(RECEIPT))?)?;
        ensure!(
            value.target == super::target()?,
            "Installation target does not match this platform"
        );
        semver::Version::parse(&value.version)?;
        ensure!(
            value.files.contains_key(PROGRAM) && value.files.len() == 1,
            "Invalid installation receipt"
        );
        for (name, digest) in &value.files {
            ensure!(
                hash(&directory.join(name))? == *digest,
                "Installed file does not match its receipt: {name}"
            );
        }
        Ok(value)
    }
}

pub(super) fn hash(path: &Path) -> Result<String> {
    let metadata = paths::no_link(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_EXPANDED,
        "Invalid program file"
    );
    let mut file = File::open(path)?.take(MAX_EXPANDED + 1);
    let mut digest = Sha256::new();
    let size = std::io::copy(&mut file, &mut digest)?;
    ensure!(size == metadata.len(), "Program changed while reading");
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn extract(archive: &Path, destination: &Path, version: &str) -> Result<Receipt> {
    let prefix = format!("gitwatch-v{version}-{}", super::target()?);
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    if archive.extension().is_some_and(|ext| ext == "zip") {
        let mut archive = zip::ZipArchive::new(File::open(archive)?)?;
        ensure!(archive.len() <= 16, "Too many files in release archive");
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let name = entry.name().to_owned();
            ensure!(
                !entry.is_dir()
                    && entry
                        .unix_mode()
                        .is_none_or(|mode| mode & 0o170000 == 0 || mode & 0o170000 == 0o100000),
                "Links and directories are not allowed in release archives"
            );
            write_entry(
                &name,
                entry.size(),
                &mut entry,
                &prefix,
                destination,
                &mut seen,
                &mut total,
            )?;
        }
    } else {
        let decoder = flate2::read::GzDecoder::new(File::open(archive)?);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            ensure!(
                entry.header().entry_type().is_file(),
                "Links and directories are not allowed in release archives"
            );
            let name = entry
                .path()?
                .to_str()
                .context("Invalid archive filename")?
                .to_owned();
            write_entry(
                &name,
                entry.size(),
                &mut entry,
                &prefix,
                destination,
                &mut seen,
                &mut total,
            )?;
        }
    }
    let receipt = Receipt::read(destination)?;
    ensure!(
        receipt.version == version,
        "Release manifest version does not match"
    );
    for name in [PROGRAM, "LICENSE", "OFL.txt", "NOTICE"] {
        ensure!(
            seen.contains(name),
            "Release archive is missing a required file"
        );
    }
    for name in receipt.files.keys() {
        verify_binary(&destination.join(name))?;
    }
    Ok(receipt)
}

fn verify_binary(path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let mut header = [0_u8; 64];
    file.read_exact(&mut header)?;
    // PE/COFF, ELF and Mach-O encode the target architecture in their headers.
    // https://learn.microsoft.com/windows/win32/debug/pe-format
    // https://refspecs.linuxfoundation.org/elf/gabi4+/ch4.eheader.html
    // https://github.com/apple-oss-distributions/xnu/blob/main/EXTERNAL_HEADERS/mach-o/loader.h
    let valid = if cfg!(windows) {
        let offset = u32::from_le_bytes(header[60..64].try_into()?);
        ensure!(
            u64::from(offset) < file.metadata()?.len(),
            "Invalid executable header"
        );
        file.seek(SeekFrom::Start(u64::from(offset)))?;
        let mut pe = [0_u8; 24];
        file.read_exact(&mut pe)?;
        header[..2] == *b"MZ"
            && pe[..4] == *b"PE\0\0"
            && pe[4..6] == [0x64, 0x86]
            && u16::from_le_bytes([pe[22], pe[23]]) & 0x2002 == 0x0002
    } else if cfg!(target_os = "linux") {
        header[..4] == *b"\x7fELF"
            && header[4] == 2
            && header[5] == 1
            && header[18..20] == [62, 0]
            && matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
    } else {
        header[..4] == [0xcf, 0xfa, 0xed, 0xfe]
            && header[4..8] == [12, 0, 0, 1]
            && header[12..16] == [2, 0, 0, 0]
    };
    ensure!(
        valid,
        "Executable architecture does not match this platform"
    );
    Ok(())
}

fn write_entry(
    name: &str,
    size: u64,
    reader: impl Read,
    prefix: &str,
    destination: &Path,
    seen: &mut HashSet<String>,
    total: &mut u64,
) -> Result<()> {
    let (directory, name) = name.split_once('/').context("Invalid archive path")?;
    ensure!(
        directory == prefix
            && [
                PROGRAM,
                RECEIPT,
                "LICENSE",
                "README.md",
                "OFL.txt",
                "NOTICE"
            ]
            .contains(&name),
        "Unexpected archive path"
    );
    ensure!(seen.insert(name.into()), "Duplicate archive path");
    *total = total.checked_add(size).context("Archive size overflow")?;
    ensure!(
        *total <= MAX_EXPANDED && size > 0,
        "Release archive expands beyond its limit"
    );
    let path = destination.join(name);
    let mut file = File::options().create_new(true).write(true).open(&path)?;
    ensure!(
        std::io::copy(&mut reader.take(size + 1), &mut file)? == size,
        "Archive entry size does not match"
    );
    file.flush()?;
    #[cfg(unix)]
    if [PROGRAM].contains(&name) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn both_archive_formats_verify_manifest_and_contents() {
        let temp = tempfile::tempdir().unwrap();
        let mut program = vec![0_u8; 88];
        if cfg!(windows) {
            program[..2].copy_from_slice(b"MZ");
            program[60] = 64;
            program[64..70].copy_from_slice(b"PE\0\0\x64\x86");
            program[86] = 2;
        } else if cfg!(target_os = "linux") {
            program[..6].copy_from_slice(b"\x7fELF\x02\x01");
            program[16] = 2;
            program[18] = 62;
        } else {
            program[..8].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe, 12, 0, 0, 1]);
            program[12] = 2;
        }
        let receipt = Receipt {
            version: "9.0.0".into(),
            target: super::super::target().unwrap().into(),
            files: [PROGRAM]
                .into_iter()
                .map(|name| (name.into(), paths::digest(&program)))
                .collect(),
        };
        let json = serde_json::to_vec(&receipt).unwrap();
        let files = [
            (PROGRAM, program.as_slice()),
            (RECEIPT, json.as_slice()),
            ("LICENSE", &b"license"[..]),
            ("OFL.txt", &b"font license"[..]),
            ("NOTICE", &b"notice"[..]),
        ];
        let prefix = format!("gitwatch-v9.0.0-{}", receipt.target);
        for extension in ["zip", "tar.gz"] {
            let path = temp.path().join(format!("release.{extension}"));
            if extension == "zip" {
                let mut archive = zip::ZipWriter::new(File::create(&path).unwrap());
                for (name, data) in files {
                    archive
                        .start_file(
                            format!("{prefix}/{name}"),
                            zip::write::SimpleFileOptions::default(),
                        )
                        .unwrap();
                    archive.write_all(data).unwrap();
                }
                archive.finish().unwrap();
            } else {
                let encoder = flate2::write::GzEncoder::new(
                    File::create(&path).unwrap(),
                    flate2::Compression::default(),
                );
                let mut archive = tar::Builder::new(encoder);
                for (name, data) in files {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(data.len() as u64);
                    header.set_mode(0o644);
                    header.set_cksum();
                    archive
                        .append_data(&mut header, format!("{prefix}/{name}"), data)
                        .unwrap();
                }
                archive.into_inner().unwrap().finish().unwrap();
            }
            let destination = temp.path().join(extension);
            fs::create_dir(&destination).unwrap();
            extract(&path, &destination, "9.0.0").unwrap();
            fs::write(destination.join(PROGRAM), "tampered").unwrap();
            assert!(Receipt::read(&destination).is_err());
            assert!(verify_binary(&destination.join(PROGRAM)).is_err());
            let mut wrong = program.clone();
            wrong[0] = 0;
            fs::write(destination.join(PROGRAM), wrong).unwrap();
            assert!(verify_binary(&destination.join(PROGRAM)).is_err());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_ne!(
                    fs::metadata(destination.join(PROGRAM))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o111,
                    0
                );
            }
        }
    }

    #[test]
    fn archive_paths_and_expansion_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut seen = HashSet::new();
        let mut size = 0;
        for name in [
            "../gitwatch",
            "release/../gitwatch",
            "release/sub/gitwatch",
            "C:/gitwatch",
            "/release/gitwatch",
        ] {
            assert!(
                write_entry(
                    name,
                    1,
                    &b"x"[..],
                    "release",
                    dir.path(),
                    &mut seen,
                    &mut size
                )
                .is_err()
            );
        }
        let name = format!("release/{PROGRAM}");
        write_entry(
            &name,
            1,
            &b"x"[..],
            "release",
            dir.path(),
            &mut seen,
            &mut size,
        )
        .unwrap();
        assert!(
            write_entry(
                &name,
                1,
                &b"x"[..],
                "release",
                dir.path(),
                &mut seen,
                &mut size
            )
            .is_err()
        );
        assert!(
            write_entry(
                "release/LICENSE",
                MAX_EXPANDED,
                &b"x"[..],
                "release",
                dir.path(),
                &mut seen,
                &mut size
            )
            .is_err()
        );
    }
}
