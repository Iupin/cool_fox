//! `code-bytes` (diagnostics builds): the bytes of a title image at an rva,
//! as the loader maps them (its prepared copy), written to a file outside
//! the repository for an external disassembler. The tree has no general
//! x86-64 disassembler (only the store decoder of the GPU side's title-write
//! emulation and the loader's decoder of console-only instructions), and
//! none is added; the writers watch (`VICTORY_WRITERS`) names code as
//! `module+rva`, which this takes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// A hex number, with or without `0x`.
pub fn hex(text: &str) -> Result<u32, String> {
    let t = text.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    u32::from_str_radix(t, 16).map_err(|e| format!("{text}: not a hex number ({e})"))
}

/// The bytes of `image` (a PE file's bytes) at `rva..rva + len` as mapped:
/// those its sections hold, zeros past their raw data; an error if the range
/// leaves the image's sections.
pub fn bytes_at(image: &[u8], rva: u32, len: u32) -> Result<Vec<u8>> {
    let info = victory_pe::ImageInfo::parse(image).context("parsing the image")?;
    let end = rva.checked_add(len).context("the range passes 4 GiB")?;
    let mut out = Vec::with_capacity(len as usize);
    for at in rva..end {
        let section = info
            .sections
            .iter()
            .find(|s| (s.rva..s.rva.saturating_add(s.virtual_size.max(s.file_size))).contains(&at));
        let Some(section) = section else {
            bail!("rva {at:#x} is in no section of the image");
        };
        out.push(
            info.rva_to_file_offset(at)
                .and_then(|offset| image.get(offset as usize).copied())
                .filter(|_| at - section.rva < section.file_size)
                .unwrap_or(0),
        );
    }
    Ok(out)
}

/// Lines of 16 bytes, each with its rva.
pub fn hex_lines(rva: u32, bytes: &[u8]) -> String {
    let mut text = String::new();
    for (i, row) in bytes.chunks(16).enumerate() {
        let hex: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
        text.push_str(&format!(
            "{:08x}: {}\n",
            rva as usize + i * 16,
            hex.join(" ")
        ));
    }
    text
}

pub fn run(file: &Path, rva: u32, len: u32, out: Option<PathBuf>) -> Result<()> {
    let original = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let prepared =
        victory_loader::prepare(&original).context("preparing the image as the loader does")?;
    let bytes = bytes_at(&prepared.bytes, rva, len)?;
    let out = match out {
        Some(out) => out,
        None => {
            let cfg = victory_core::Config::load()?;
            let stem = file.file_stem().unwrap_or_default().to_string_lossy();
            cfg.cache_subdir("code")?
                .join(format!("{stem}-{rva:x}-{len:x}.bin"))
        }
    };
    victory_core::ensure_outside_repo(&out)?;
    std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;
    let text = out.with_extension("txt");
    std::fs::write(&text, hex_lines(rva, &bytes))
        .with_context(|| format!("writing {}", text.display()))?;
    println!("{len:#x} bytes of {} at rva {rva:#x}", file.display());
    println!("raw            {}", out.display());
    println!("hex            {}", text.display());
    println!(
        "disassemble    objdump -D -b binary -m i386:x86-64 -M intel --adjust-vma={rva:#x} \"{}\"",
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_takes_both_forms() {
        assert_eq!(hex("0x1A2b"), Ok(0x1a2b));
        assert_eq!(hex("ff"), Ok(0xff));
        assert!(hex("xyz").is_err());
    }

    #[test]
    fn lines_name_their_rva() {
        let bytes: Vec<u8> = (0..20).collect();
        let text = hex_lines(0x1000, &bytes);
        assert_eq!(
            text,
            "00001000: 00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f\n00001010: 10 11 12 13\n"
        );
    }

    /// The bytes at an rva of this test's own executable are its mapped
    /// bytes there; a range past the sections is refused.
    #[test]
    fn bytes_are_read_as_mapped() {
        let exe = std::env::current_exe().unwrap();
        let image = std::fs::read(&exe).unwrap();
        let info = victory_pe::ImageInfo::parse(&image).unwrap();
        let text = info.sections.iter().find(|s| s.name == ".text").unwrap();
        let bytes = bytes_at(&image, text.rva + 0x10, 32).unwrap();
        let offset = info.rva_to_file_offset(text.rva + 0x10).unwrap() as usize;
        assert_eq!(bytes, image[offset..offset + 32]);
        let last = info
            .sections
            .iter()
            .map(|s| s.rva + s.virtual_size.max(s.file_size))
            .max()
            .unwrap();
        assert!(bytes_at(&image, last, 4).is_err());
    }
}
