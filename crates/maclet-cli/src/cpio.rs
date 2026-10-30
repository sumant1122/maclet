use std::io::{Result, Write};

/// Writes a cpio archive in SVR4 portable format with CRC/newc ("070701")
pub struct CpioWriter<W: Write> {
    writer: W,
    ino: u32,
}

impl<W: Write> CpioWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer, ino: 1 }
    }

    pub fn write_entry(
        &mut self,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        data: &[u8],
    ) -> Result<()> {
        let name_bytes = name.as_bytes();
        let name_len_with_null = name_bytes.len() + 1;

        let header = format!(
            "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}00000000",
            self.ino,
            mode,
            uid,
            gid,
            1, // nlink
            0, // mtime
            data.len(),
            0, // devmajor
            0, // devminor
            0, // rdevmajor
            0, // rdevminor
            name_len_with_null
        );

        self.writer.write_all(header.as_bytes())?;
        self.writer.write_all(name_bytes)?;
        self.writer.write_all(&[0u8])?; // null terminator

        // Pad header + name to 4-byte boundary
        let total_hdr = 110 + name_len_with_null;
        let pad_hdr = (4 - (total_hdr % 4)) % 4;
        if pad_hdr > 0 {
            self.writer.write_all(&vec![0u8; pad_hdr])?;
        }

        // Write file data
        if !data.is_empty() {
            self.writer.write_all(data)?;
            let pad_data = (4 - (data.len() % 4)) % 4;
            if pad_data > 0 {
                self.writer.write_all(&vec![0u8; pad_data])?;
            }
        }

        self.ino += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        self.write_entry("TRAILER!!!", 0, 0, 0, &[])?;
        // Pad archive to 512-byte block
        self.writer.flush()?;
        Ok(self.writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpio_entry() {
        let mut buf = Vec::new();
        let mut cpio = CpioWriter::new(&mut buf);
        cpio.write_entry("init", 0o100755, 0, 0, b"echo hello").unwrap();
        let _ = cpio.finish().unwrap();
        assert!(buf.starts_with(b"070701"));
        assert!(buf.windows(10).any(|w| w == b"TRAILER!!!"));
    }
}
