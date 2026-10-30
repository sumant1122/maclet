use maclet_common::{MacletError, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::io::{BufRead, Write};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// Reads a single newline-terminated JSON message synchronously
pub fn read_sync<R: BufRead, T: DeserializeOwned>(reader: &mut R) -> Result<Option<T>> {
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line)?;
    if bytes_read == 0 {
        return Ok(None);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let msg = serde_json::from_str::<T>(trimmed)
        .map_err(|e| MacletError::Protocol(format!("Failed to parse line '{trimmed}': {e}")))?;
    Ok(Some(msg))
}

/// Writes a single newline-terminated JSON message synchronously
pub fn write_sync<W: Write, T: Serialize>(writer: &mut W, msg: &T) -> Result<()> {
    let mut data = serde_json::to_vec(msg)?;
    data.push(b'\n');
    writer.write_all(&data)?;
    writer.flush()?;
    Ok(())
}

/// Reads a single newline-terminated JSON message asynchronously
pub async fn read_async<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncBufReadExt + Unpin,
    T: DeserializeOwned,
{
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line).await?;
    if bytes_read == 0 {
        return Ok(None);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let msg = serde_json::from_str::<T>(trimmed)
        .map_err(|e| MacletError::Protocol(format!("Failed to parse line '{trimmed}': {e}")))?;
    Ok(Some(msg))
}

/// Writes a single newline-terminated JSON message asynchronously
pub async fn write_async<W, T>(writer: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let mut data = serde_json::to_vec(msg)?;
    data.push(b'\n');
    writer.write_all(&data).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[derive(Debug, Serialize, serde::Deserialize, PartialEq)]
    struct Sample {
        text: String,
        num: u32,
    }

    #[test]
    fn test_sync_roundtrip() {
        let msg = Sample {
            text: "hello".to_string(),
            num: 42,
        };
        let mut buf = Vec::new();
        write_sync(&mut buf, &msg).unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: Sample = read_sync(&mut cursor).unwrap().unwrap();
        assert_eq!(msg, decoded);
    }

    #[tokio::test]
    async fn test_async_roundtrip() {
        let msg = Sample {
            text: "async-test".to_string(),
            num: 99,
        };
        let mut buf = Vec::new();
        write_async(&mut buf, &msg).await.unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: Sample = read_async(&mut cursor).await.unwrap().unwrap();
        assert_eq!(msg, decoded);
    }
}
