use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

pub async fn read_frame<R>(stream: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    let mut read = 0usize;
    while read < len_buf.len() {
        let n = stream.read(&mut len_buf[read..]).await?;
        if n == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partial frame header"));
        }
        read += n;
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    let mut read = 0usize;
    while read < buf.len() {
        let n = stream.read(&mut buf[read..]).await?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partial frame body"));
        }
        read += n;
    }
    Ok(Some(buf))
}

pub async fn write_frame<W>(stream: &mut W, payload: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(payload).await?;
    Ok(())
}
