//! HTTP archive transport. Extraction produces an ordinary source tree; skill
//! discovery and installation remain responsibilities of the common source flow.

use crate::ops::DownloadDir;
use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const MAX_DOWNLOAD: u64 = 128 * 1024 * 1024;
use super::validate::{MAX_ENTRIES, MAX_EXPANDED};
const MAX_METADATA: u64 = 1024 * 1024;
const XZ_MEMORY_LIMIT_KIB: u32 = 128 * 1024;

/// Download and extract into an absent or empty directory. Failed downloads or
/// archives never publish a partial tree into the caller's destination.
pub fn download(url: &str, destination: &Path) -> Result<()> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        bail!("archive URLs must use HTTP or HTTPS");
    }
    require_empty_destination(destination)?;
    let work = DownloadDir::new("archive")?;
    let payload = work.path().join("download");
    download_file(url, &payload, MAX_DOWNLOAD)?;
    let extracted = work.path().join("extracted");
    let mut directory = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(&extracted)?;
    extract(&payload, &extracted, MAX_EXPANDED, MAX_ENTRIES)?;
    // Both paths are temporary directories on the system temporary filesystem.
    // rename also refuses to replace a destination that acquired any contents.
    fs::rename(&extracted, destination).context("publishing extracted source")?;
    Ok(())
}

fn require_empty_destination(destination: &Path) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            if fs::read_dir(destination)?.next().is_some() {
                bail!("archive destination must be empty");
            }
        }
        Ok(_) => bail!("archive destination must be a directory, not a file or link"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn download_file(url: &str, path: &Path, limit: u64) -> Result<()> {
    let error_path = path.with_extension("stderr");
    let stderr = File::create(&error_path)?;
    let mut file = File::create(path)?;
    let mut child = Command::new("curl")
        // Ignore user curlrc output/hooks, and restrict redirects as well as the
        // initial request so an HTTP response cannot switch to a local file URL.
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--globoff",
            "--max-redirs",
            "5",
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--connect-timeout",
            "15",
            "--max-time",
            "180",
            "--speed-time",
            "30",
            "--speed-limit",
            "1024",
            "--max-filesize",
            &limit.to_string(),
            "--url",
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .context("downloading archive requires curl")?;
    let result = copy_limited(
        child.stdout.take().expect("piped curl stdout"),
        &mut file,
        limit,
    );
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().context("waiting for archive download")?;
    result.context("archive download exceeds its size limit or could not be written")?;
    if !status.success() {
        let mut detail = String::new();
        File::open(error_path)?
            .take(4096)
            .read_to_string(&mut detail)?;
        let detail: String = detail.chars().filter(|c| !c.is_control()).collect();
        bail!("archive download failed: {}", detail.trim());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Format {
    Zip,
    Tar,
    Gzip,
    Bzip2,
    Xz,
}

fn detect(file: &mut File) -> Result<Format> {
    let mut header = [0; 512];
    let size = file.read(&mut header)?;
    file.rewind()?;
    detect_bytes(&header[..size])
        .context("unsupported archive; expected ZIP, TAR, TAR.GZ, TAR.BZ2, or TAR.XZ")
}

fn detect_bytes(bytes: &[u8]) -> Option<Format> {
    let format = if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        Format::Zip
    } else if bytes.starts_with(b"\x1f\x8b") {
        Format::Gzip
    } else if bytes.starts_with(b"BZh") {
        Format::Bzip2
    } else if bytes.starts_with(b"\xfd7zXZ\0") {
        Format::Xz
    } else if bytes.len() >= 512 && tar_header_valid(bytes[..512].try_into().ok()?) {
        Format::Tar
    } else {
        return None;
    };
    Some(format)
}

fn tar_header_valid(header: &[u8; 512]) -> bool {
    let field = String::from_utf8_lossy(&header[148..156]);
    let Ok(expected) = u64::from_str_radix(field.trim_matches(['\0', ' ']), 8) else {
        return false;
    };
    let actual: u64 = header
        .iter()
        .enumerate()
        .map(|(i, byte)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(*byte)
            }
        })
        .sum();
    actual == expected
}

fn extract(payload: &Path, destination: &Path, byte_limit: u64, entry_limit: usize) -> Result<()> {
    let mut file = File::open(payload)?;
    match detect(&mut file)? {
        Format::Zip => extract_zip(file, destination, byte_limit, entry_limit),
        Format::Tar => extract_tar(file, destination, byte_limit, entry_limit),
        format => {
            let tar_path = payload.with_extension("tar");
            let mut tar_file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&tar_path)?;
            // Bound the entire decoded stream, including TAR metadata and padding;
            // reading to EOF also verifies the compression checksum and trailer.
            match format {
                Format::Gzip => {
                    copy_limited(
                        flate2::read::MultiGzDecoder::new(file),
                        &mut tar_file,
                        byte_limit,
                    )?;
                }
                Format::Bzip2 => {
                    copy_limited(
                        bzip2::read::MultiBzDecoder::new(file),
                        &mut tar_file,
                        byte_limit,
                    )?;
                }
                Format::Xz => decode_xz(file, &mut tar_file, byte_limit)?,
                _ => unreachable!(),
            }
            tar_file.rewind()?;
            extract_tar(tar_file, destination, byte_limit, entry_limit)
        }
    }
}

fn decode_xz(mut reader: impl Read, mut writer: impl Write, limit: u64) -> Result<()> {
    use lzma_rust2::{Action, Status, XzStream};
    let mut decoder = XzStream::new_mem_limit(true, XZ_MEMORY_LIMIT_KIB);
    let mut input = [0u8; 64 * 1024];
    let mut output = [0u8; 64 * 1024];
    let (mut position, mut length, mut total) = (0, 0, 0u64);
    let mut eof = false;
    loop {
        if position == length && !eof {
            length = reader.read(&mut input)?;
            position = 0;
            eof = length == 0;
        }
        let action = if eof { Action::Finish } else { Action::Run };
        let result = decoder.process(&input[position..length], &mut output, action)?;
        position += result.bytes_consumed;
        total += result.bytes_produced as u64;
        if total > limit {
            bail!("expanded archive exceeds {limit} bytes");
        }
        writer.write_all(&output[..result.bytes_produced])?;
        if result.status == Status::StreamEnd {
            return Ok(());
        }
        if result.bytes_consumed == 0 && result.bytes_produced == 0 && (eof || position < length) {
            bail!("truncated or invalid XZ archive");
        }
    }
}

fn extract_tar(
    mut file: File,
    destination: &Path,
    byte_limit: u64,
    entry_limit: usize,
) -> Result<()> {
    // TAR long-name/PAX records are normally consumed before yielding an entry.
    // Inspect raw records first so metadata cannot bypass count and memory limits.
    let mut raw = tar::Archive::new(&mut file);
    for (index, entry) in raw.entries()?.raw(true).enumerate() {
        let entry = entry.context("reading TAR header")?;
        if index >= entry_limit {
            bail!("archive contains too many entries (limit {entry_limit})");
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() && entry.size() > MAX_METADATA {
            bail!("archive metadata is too large");
        }
    }
    file.rewind()?;
    let mut archive = tar::Archive::new(BufReader::new(file));
    let mut remaining = byte_limit;
    for entry in archive.entries()? {
        let mut entry = entry.context("reading TAR entry")?;
        let relative = safe_path(entry.path()?.as_ref())?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            create_directory(destination, &relative)?;
        } else if kind.is_file() {
            let size = entry.size();
            let mode = entry.header().mode()?;
            write_file(
                destination,
                &relative,
                &mut entry,
                size,
                mode,
                &mut remaining,
            )?;
        } else {
            bail!(
                "archive contains a link or unsupported entry: {}",
                relative.display()
            );
        }
    }
    Ok(())
}

fn extract_zip(
    mut file: File,
    destination: &Path,
    byte_limit: u64,
    entry_limit: usize,
) -> Result<()> {
    let declared_entries = zip_entry_count(&mut file, entry_limit)?;
    let mut archive = zip::ZipArchive::new(file).context("reading ZIP archive")?;
    if archive.len() != declared_entries {
        bail!("ZIP archive contains duplicate or inconsistent entries");
    }
    let mut ranges = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        if entry.compressed_size() == 0 {
            continue;
        }
        let start = entry.data_start().context("ZIP entry has no data offset")?;
        let end = start
            .checked_add(entry.compressed_size())
            .context("invalid ZIP data range")?;
        ranges.push((start, end));
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        bail!("ZIP archive contains overlapping file data");
    }
    let mut remaining = byte_limit;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).context("reading ZIP entry")?;
        let relative = safe_path(Path::new(entry.name()))?;
        let mode = entry.unix_mode().unwrap_or(0);
        let kind = mode & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 {
            bail!(
                "archive contains a link or unsupported entry: {}",
                relative.display()
            );
        }
        if entry.is_dir() {
            create_directory(destination, &relative)?;
        } else {
            let size = entry.size();
            write_file(
                destination,
                &relative,
                &mut entry,
                size,
                mode,
                &mut remaining,
            )?;
        }
    }
    Ok(())
}

fn zip_entry_count(file: &mut File, limit: usize) -> Result<usize> {
    // ZipArchive allocates its index before exposing len(). Bound the declared
    // count and ZIP64 metadata before handing the file to that parser.
    let length = file.metadata()?.len();
    let tail_length = length.min(22 + 65_535 + 20) as usize;
    let mut tail = vec![0; tail_length];
    file.seek(SeekFrom::End(-(tail_length as i64)))?;
    file.read_exact(&mut tail)?;
    let footer = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| {
            tail[at..].starts_with(b"PK\x05\x06")
                && at + 22 + usize::from(le16(&tail, at + 20)) == tail.len()
        })
        .context("ZIP archive has no complete end record")?;
    if le16(&tail, footer + 4) != 0 || le16(&tail, footer + 6) != 0 {
        bail!("split ZIP archives are not supported");
    }
    let mut count = u64::from(le16(&tail, footer + 10));
    if count != u64::from(le16(&tail, footer + 8)) {
        bail!("ZIP archive has inconsistent entry counts");
    }
    let may_be_zip64 = count == u64::from(u16::MAX)
        || le32(&tail, footer + 12) == u32::MAX
        || le32(&tail, footer + 16) == u32::MAX;
    if may_be_zip64 && footer >= 20 && tail[footer - 20..].starts_with(b"PK\x06\x07") {
        let locator = &tail[footer - 20..footer];
        if le32(locator, 4) != 0 || le32(locator, 16) != 1 {
            bail!("split ZIP64 archives are not supported");
        }
        file.seek(SeekFrom::Start(le64(locator, 8)))?;
        let mut header = [0; 56];
        file.read_exact(&mut header)
            .context("reading ZIP64 end record")?;
        if !header.starts_with(b"PK\x06\x06") || !(44..=MAX_METADATA).contains(&le64(&header, 4)) {
            bail!("ZIP64 end record is invalid or too large");
        }
        if le32(&header, 16) != 0
            || le32(&header, 20) != 0
            || le64(&header, 24) != le64(&header, 32)
        {
            bail!("split ZIP64 archives are not supported");
        }
        count = le64(&header, 32);
    }
    if count > limit as u64 {
        bail!("archive contains too many entries (limit {limit})");
    }
    validate_zip_end_records(file, length, limit)?;
    file.rewind()?;
    Ok(count as usize)
}

fn validate_zip_end_records(file: &mut File, length: u64, limit: usize) -> Result<()> {
    // The ZIP parser retries earlier end records after malformed directories.
    // Bound every candidate it could use, including records inside stored ZIP
    // resources, so fallback cannot bypass the selected footer's limits.
    const WINDOW: usize = 64 * 1024;
    let mut buffer = [0u8; WINDOW + 55];
    let mut offset = 0;
    while offset < length {
        let available = (length - offset).min(buffer.len() as u64) as usize;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buffer[..available])?;
        for at in 0..available.min(WINDOW) {
            let bytes = &buffer[at..available];
            let position = offset + at as u64;
            if bytes.len() >= 22 && bytes.starts_with(b"PK\x05\x06") {
                let complete = position + 22 + u64::from(le16(bytes, 20)) <= length;
                let may_be_zip64 = le16(bytes, 10) == u16::MAX
                    || le32(bytes, 12) == u32::MAX
                    || le32(bytes, 16) == u32::MAX;
                let uses_zip64 = may_be_zip64 && zip64_locator_at(file, position)?;
                if complete
                    && !uses_zip64
                    && le16(bytes, 4) == le16(bytes, 6)
                    && usize::from(le16(bytes, 8)) > limit
                {
                    bail!("ZIP end record exceeds the entry limit ({limit})");
                }
            }
            if bytes.len() >= 56 && bytes.starts_with(b"PK\x06\x06") {
                let size = le64(bytes, 4);
                let complete = size >= 40 && size.saturating_add(12) <= length - position;
                if complete && (size > MAX_METADATA || le64(bytes, 32) > limit as u64) {
                    bail!("ZIP64 end record exceeds the metadata or entry limit");
                }
            }
        }
        offset += available.min(WINDOW) as u64;
    }
    Ok(())
}

fn zip64_locator_at(file: &mut File, footer: u64) -> Result<bool> {
    if footer < 20 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(footer - 20))?;
    let mut signature = [0; 4];
    file.read_exact(&mut signature)?;
    Ok(signature == *b"PK\x06\x07")
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().expect("ZIP field width"))
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("ZIP field width"))
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("ZIP field width"))
}

fn safe_path(path: &Path) -> Result<PathBuf> {
    let text = path.to_str().context("archive path is not UTF-8")?;
    if text.contains(['\\', ':']) || text.chars().any(char::is_control) {
        bail!("unsafe archive path: {path:?}");
    }
    let mut relative = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            _ => bail!("unsafe archive path: {path:?}"),
        }
    }
    if relative.components().count() > 64 || text.len() > 4096 {
        bail!("archive path is too long");
    }
    Ok(relative)
}

fn create_directory(destination: &Path, relative: &Path) -> Result<()> {
    fs::create_dir_all(destination.join(relative)).context("creating archive directory")
}

fn write_file(
    destination: &Path,
    relative: &Path,
    reader: impl Read,
    expected: u64,
    mode: u32,
    remaining: &mut u64,
) -> Result<()> {
    if relative.as_os_str().is_empty() || expected > *remaining {
        bail!("archive file is invalid or exceeds the expanded size limit");
    }
    let path = destination.join(relative);
    create_directory(
        destination,
        relative.parent().context("archive file has no parent")?,
    )?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| {
            format!(
                "creating archive file {} (duplicate paths are not allowed)",
                relative.display()
            )
        })?;
    let actual = copy_limited(reader, &mut file, expected).context("extracting archive file")?;
    if actual != expected {
        bail!("archive entry is truncated: {}", relative.display());
    }
    *remaining -= actual;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o644 | (mode & 0o111)))?;
    }
    Ok(())
}

fn copy_limited(mut reader: impl Read, mut writer: impl Write, limit: u64) -> Result<u64> {
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(total);
        }
        total += count as u64;
        if total > limit {
            bail!("size limit exceeded ({limit} bytes)");
        }
        writer.write_all(&buffer[..count])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    fn tar(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, contents, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_entry_type(*kind);
            header.set_path(name).unwrap();
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_link_name("../../outside").unwrap();
            }
            header.set_cksum();
            builder.append(&header, Cursor::new(contents)).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);
        for (name, content) in entries {
            writer.start_file(name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn fixture() -> Vec<u8> {
        tar(&[(
            "bundle/skills/hello/SKILL.md",
            b"hello",
            tar::EntryType::Regular,
        )])
    }

    fn xz(bytes: &[u8]) -> Vec<u8> {
        let mut writer =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap()
    }

    fn extract_fixture(
        bytes: &[u8],
        byte_limit: u64,
        entry_limit: usize,
    ) -> (DownloadDir, Result<()>) {
        let work = DownloadDir::new("archive-test").unwrap();
        let file = work.path().join("opaque-download");
        let tree = work.path().join("tree");
        fs::write(&file, bytes).unwrap();
        fs::create_dir(&tree).unwrap();
        let result = extract(&file, &tree, byte_limit, entry_limit);
        (work, result)
    }

    #[test]
    fn recognizes_content_without_filename_or_content_type() {
        let tar = fixture();
        let zip = zip(&[("bundle/skills/hello/SKILL.md", b"hello")]);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar).unwrap();
        let gzip = gzip.finish().unwrap();
        let mut bzip = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        bzip.write_all(&tar).unwrap();
        let bzip = bzip.finish().unwrap();
        let xz = xz(&tar);
        for bytes in [tar, zip, gzip, bzip, xz] {
            assert!(detect_bytes(&bytes).is_some());
            let (work, result) = extract_fixture(&bytes, MAX_EXPANDED, MAX_ENTRIES);
            result.unwrap();
            let file = work.path().join("tree/bundle/skills/hello/SKILL.md");
            assert_eq!(fs::read(&file).unwrap(), b"hello");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(file).unwrap().permissions().mode() & 0o7777,
                    0o755
                );
            }
        }
    }

    #[test]
    fn rejects_unsafe_paths_and_archive_links() {
        for name in [
            "../escape",
            "/absolute",
            "a/../../escape",
            "C:/escape",
            "a\\escape",
            "a\nescape",
        ] {
            let (_, result) = extract_fixture(&zip(&[(name, b"bad")]), MAX_EXPANDED, MAX_ENTRIES);
            assert!(result.is_err(), "accepted {name:?}");
        }
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Fifo,
        ] {
            let (_, result) =
                extract_fixture(&tar(&[("unsafe", b"", kind)]), MAX_EXPANDED, MAX_ENTRIES);
            assert!(result.is_err(), "accepted {kind:?}");
        }
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(
                "unsafe",
                "../../outside",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        let (_, result) = extract_fixture(
            &writer.finish().unwrap().into_inner(),
            MAX_EXPANDED,
            MAX_ENTRIES,
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_duplicate_files_and_truncated_contents() {
        let repeated = tar(&[
            ("same", b"first", tar::EntryType::Regular),
            ("same", b"second", tar::EntryType::Regular),
        ]);
        let (_, result) = extract_fixture(&repeated, MAX_EXPANDED, MAX_ENTRIES);
        assert!(result.unwrap_err().to_string().contains("duplicate"));
        let mut truncated = fixture();
        truncated.truncate(515);
        let (_, result) = extract_fixture(&truncated, MAX_EXPANDED, MAX_ENTRIES);
        assert!(result.is_err());
        let (_, result) = extract_fixture(
            b"<html>sign in to download</html>",
            MAX_EXPANDED,
            MAX_ENTRIES,
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported archive")
        );
    }

    #[test]
    fn bounds_archive_entries_and_actual_expansion() {
        for bytes in [fixture(), zip(&[("one", b"12345")])] {
            assert!(extract_fixture(&bytes, 4, MAX_ENTRIES).1.is_err());
            assert!(extract_fixture(&bytes, MAX_EXPANDED, 0).1.is_err());
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gzip.write_all(&fixture()).unwrap();
        assert!(
            extract_fixture(&gzip.finish().unwrap(), 1024, MAX_ENTRIES)
                .1
                .is_err()
        );
        let mut bytes = Vec::new();
        assert!(copy_limited(Cursor::new([0; 64]), &mut bytes, 32).is_err());
        assert!(bytes.len() <= 32);
    }

    #[test]
    fn zip_index_limits_duplicates_and_overlap_are_checked_before_extraction() {
        let original = zip(&[("one", b"hello"), ("two", b"world")]);
        let central: Vec<_> = original
            .windows(4)
            .enumerate()
            .filter_map(|(at, bytes)| (bytes == b"PK\x01\x02").then_some(at))
            .collect();
        assert_eq!(central.len(), 2);
        let footer = original.len() - 22;
        let mut excessive = original.clone();
        excessive[footer + 8..footer + 10].copy_from_slice(&30_000u16.to_le_bytes());
        excessive[footer + 10..footer + 12].copy_from_slice(&30_000u16.to_le_bytes());
        let error = extract_fixture(&excessive, MAX_EXPANDED, MAX_ENTRIES)
            .1
            .unwrap_err();
        assert!(error.to_string().contains("too many entries"));

        let mut duplicate = original.clone();
        duplicate[central[1] + 46..central[1] + 49].copy_from_slice(b"one");
        let error = extract_fixture(&duplicate, MAX_EXPANDED, MAX_ENTRIES)
            .1
            .unwrap_err();
        assert!(error.to_string().contains("duplicate"));

        let mut overlapping = original;
        overlapping[central[1] + 42..central[1] + 46].copy_from_slice(&0u32.to_le_bytes());
        let error = extract_fixture(&overlapping, MAX_EXPANDED, MAX_ENTRIES)
            .1
            .unwrap_err();
        assert!(error.to_string().contains("overlapping"));
    }

    #[test]
    fn bounded_zip64_archives_are_supported() {
        let mut bytes = zip(&[("skill/SKILL.md", b"hello")]);
        let footer = bytes.split_off(bytes.len() - 22);
        let zip64_offset = bytes.len() as u64;
        let mut zip64 = [0u8; 56];
        zip64[..4].copy_from_slice(b"PK\x06\x06");
        zip64[4..12].copy_from_slice(&44u64.to_le_bytes());
        zip64[12..14].copy_from_slice(&45u16.to_le_bytes());
        zip64[14..16].copy_from_slice(&45u16.to_le_bytes());
        zip64[24..32].copy_from_slice(&1u64.to_le_bytes());
        zip64[32..40].copy_from_slice(&1u64.to_le_bytes());
        zip64[40..48].copy_from_slice(&u64::from(le32(&footer, 12)).to_le_bytes());
        zip64[48..56].copy_from_slice(&u64::from(le32(&footer, 16)).to_le_bytes());
        bytes.extend_from_slice(&zip64);
        bytes.extend_from_slice(b"PK\x06\x07");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&zip64_offset.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let mut footer = footer;
        footer[8..12].fill(0xff);
        footer[12..20].fill(0xff);
        bytes.extend_from_slice(&footer);
        let (work, result) = extract_fixture(&bytes, MAX_EXPANDED, MAX_ENTRIES);
        result.unwrap();
        assert_eq!(
            fs::read(work.path().join("tree/skill/SKILL.md")).unwrap(),
            b"hello"
        );
        bytes[zip64_offset as usize + 24..zip64_offset as usize + 32]
            .copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[zip64_offset as usize + 32..zip64_offset as usize + 40]
            .copy_from_slice(&u64::MAX.to_le_bytes());
        let error = extract_fixture(&bytes, MAX_EXPANDED, MAX_ENTRIES)
            .1
            .unwrap_err();
        assert!(error.to_string().contains("too many entries"));
    }

    #[test]
    fn earlier_zip_end_records_cannot_bypass_index_and_metadata_limits() {
        let original = zip(&[("skill/SKILL.md", b"hello")]);
        let footer = &original[original.len() - 22..];
        let mut misleading_footer = footer.to_vec();
        misleading_footer[16..20].copy_from_slice(&0u32.to_le_bytes());

        let mut earlier = footer.to_vec();
        earlier[8..10].copy_from_slice(&30_000u16.to_le_bytes());
        earlier[10..12].copy_from_slice(&30_000u16.to_le_bytes());
        let mut bytes = original.clone();
        // Exercise a candidate spanning the fixed scan-window boundary.
        bytes.resize(64 * 1024 - 2, 0);
        bytes.extend_from_slice(&earlier);
        bytes.extend_from_slice(&misleading_footer);
        let error = extract_fixture(&bytes, MAX_EXPANDED, MAX_ENTRIES)
            .1
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("ZIP end record exceeds the entry limit")
        );

        for (record_size, count) in [(44u64, u64::MAX), (MAX_METADATA + 1, 1)] {
            let mut bytes = original.clone();
            let mut header = [0u8; 56];
            header[..4].copy_from_slice(b"PK\x06\x06");
            header[4..12].copy_from_slice(&record_size.to_le_bytes());
            header[24..32].copy_from_slice(&count.to_le_bytes());
            header[32..40].copy_from_slice(&count.to_le_bytes());
            bytes.extend_from_slice(&header);
            bytes.resize(bytes.len() + record_size as usize - 44, 0);
            bytes.extend_from_slice(&misleading_footer);
            let error = extract_fixture(&bytes, MAX_EXPANDED, MAX_ENTRIES)
                .1
                .unwrap_err();
            assert!(error.to_string().contains("ZIP64 end record exceeds"));
        }
    }

    #[test]
    fn stored_nested_zip_resources_are_preserved() {
        let inner = zip(&[("reference.txt", b"nested resource")]);
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("skill/SKILL.md", options).unwrap();
        writer.write_all(b"hello").unwrap();
        writer
            .start_file("skill/resources/examples.zip", options)
            .unwrap();
        writer.write_all(&inner).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let (work, result) = extract_fixture(&bytes, MAX_EXPANDED, MAX_ENTRIES);
        result.unwrap();
        assert_eq!(
            fs::read(work.path().join("tree/skill/resources/examples.zip")).unwrap(),
            inner
        );
    }

    #[test]
    fn tar_extended_paths_and_metadata_obey_the_same_limits() {
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_pax_extensions([("path", b"../escape".as_slice())])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_path("safe").unwrap();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, b"x".as_slice()).unwrap();
        let error = extract_fixture(&builder.into_inner().unwrap(), MAX_EXPANDED, MAX_ENTRIES)
            .1
            .unwrap_err();
        assert!(error.to_string().contains("unsafe archive path"));

        let mut header = tar::Header::new_gnu();
        header.set_path("metadata").unwrap();
        header.set_entry_type(tar::EntryType::XHeader);
        header.set_size(MAX_METADATA + 1);
        header.set_cksum();
        let (_, result) = extract_fixture(header.as_bytes(), MAX_EXPANDED, MAX_ENTRIES);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("metadata is too large")
        );
    }

    #[test]
    fn xz_streams_are_bounded_and_verify_the_trailer() {
        let content: Vec<_> = (0..200_000).map(|n| (n % 251) as u8).collect();
        let compressed = xz(&content);
        let mut output = Vec::new();
        decode_xz(Cursor::new(&compressed), &mut output, MAX_EXPANDED).unwrap();
        assert_eq!(output, content);
        assert!(decode_xz(Cursor::new(&compressed), io::sink(), 100).is_err());
        assert!(
            decode_xz(
                Cursor::new(&compressed[..compressed.len() - 1]),
                io::sink(),
                MAX_EXPANDED
            )
            .is_err()
        );
        let joined = [compressed.clone(), compressed].concat();
        let mut output = Vec::new();
        decode_xz(Cursor::new(joined), &mut output, MAX_EXPANDED).unwrap();
        assert_eq!(output.len(), content.len() * 2);
    }

    fn server(responses: Vec<Vec<u8>>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            for response in responses {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(error) => panic!("HTTP fixture accept: {error}"),
                    }
                };
                // Accepted sockets inherit nonblocking mode on macOS.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                requests.push(String::from_utf8(request).unwrap());
                let _ = stream.write_all(&response);
            }
            requests
        });
        (format!("http://{address}"), handle)
    }

    fn response(bytes: &[u8]) -> Vec<u8> {
        let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).into_bytes();
        response.extend_from_slice(bytes);
        response
    }

    #[test]
    fn downloads_redirects_and_preserves_query_strings() {
        let archive = zip(&[("skill/SKILL.md", b"hello")]);
        let redirect = b"HTTP/1.1 302 Found\r\nLocation: /download?token=a@b\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (url, server) = server(vec![redirect, response(&archive)]);
        let work = DownloadDir::new("archive-test").unwrap();
        let destination = work.path().join("destination");
        fs::create_dir(&destination).unwrap();
        download(&format!("{url}/opaque?token=x@y"), &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("skill/SKILL.md")).unwrap(),
            b"hello"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /opaque?token=x@y HTTP/"));
        assert!(requests[1].starts_with("GET /download?token=a@b HTTP/"));
    }

    #[test]
    fn failed_archive_or_http_response_does_not_publish_partial_tree() {
        let bad_archive = zip(&[("good", b"first"), ("../escape", b"bad")]);
        for data in [
            response(&bad_archive),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        ] {
            let (url, server) = server(vec![data]);
            let work = DownloadDir::new("archive-test").unwrap();
            let destination = work.path().join("destination");
            fs::create_dir(&destination).unwrap();
            assert!(download(&url, &destination).is_err());
            assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
            assert!(!work.path().join("escape").exists());
            server.join().unwrap();
        }
    }

    #[test]
    fn preserves_existing_destination_and_rejects_file_redirect() {
        let work = DownloadDir::new("archive-test").unwrap();
        fs::write(work.path().join("existing"), b"keep").unwrap();
        assert!(download("http://127.0.0.1:1/no-request", work.path()).is_err());
        assert_eq!(fs::read(work.path().join("existing")).unwrap(), b"keep");
        assert!(download("file:///etc/passwd", &work.path().join("new")).is_err());
        let (url, server) = server(vec![b"HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()]);
        assert!(download(&url, &work.path().join("new")).is_err());
        assert!(!work.path().join("new").exists());
        server.join().unwrap();
    }
}
