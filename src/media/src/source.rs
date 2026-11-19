//! Random-access, bounded reads over the evidence. Parsers never read through a `File` directly: every
//! read goes through [`Source`], which refuses reads past the real end of the file and caps allocations.

use std::io::{Read, Seek, SeekFrom};

/// Read-only random access with a known length.
pub trait Source {
    /// Real length of the data in bytes.
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Reads up to `buf.len()` bytes at `offset`; returns how many were read (0 at or past the end).
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize>;

    /// Reads exactly `n` bytes at `offset`, or None when they are not all in the file or `n > cap`.
    /// The allocation is bounded by `min(n, cap)` and by the bytes that really exist, so a hostile size
    /// field can never make the parser allocate more than the file holds.
    fn read_vec(&mut self, offset: u64, n: u64, cap: u64) -> Option<Vec<u8>> {
        if n > cap || offset.checked_add(n)? > self.len() {
            return None;
        }
        let mut v = vec![0u8; n as usize];
        let mut done = 0usize;
        while done < v.len() {
            match self.read_at(offset + done as u64, &mut v[done..]) {
                Ok(0) | Err(_) => return None,
                Ok(k) => done += k,
            }
        }
        Some(v)
    }

    /// Reads up to `n` bytes (fewer at the end of the file), capped at `cap`.
    fn read_upto(&mut self, offset: u64, n: u64, cap: u64) -> Vec<u8> {
        let avail = self.len().saturating_sub(offset);
        let n = n.min(cap).min(avail);
        self.read_vec(offset, n, cap).unwrap_or_default()
    }
}

/// Reads a fixed-size array at `offset` (None when the bytes are not all in the file).
pub fn read_array<const N: usize>(src: &mut dyn Source, offset: u64) -> Option<[u8; N]> {
    src.read_vec(offset, N as u64, N as u64)?.try_into().ok()
}

/// An in-memory buffer (tests, fuzzing).
pub struct SliceSource<'a> {
    data: &'a [u8],
}

impl<'a> SliceSource<'a> {
    pub fn new(data: &'a [u8]) -> SliceSource<'a> {
        SliceSource { data }
    }
}

impl Source for SliceSource<'_> {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.data.len() as u64 {
            return Ok(0);
        }
        let start = offset as usize;
        let n = buf.len().min(self.data.len() - start);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        Ok(n)
    }
}

/// A read-only file with a small read-ahead window (frame and page walks issue many tiny reads).
pub struct FileSource<R: Read + Seek> {
    inner: R,
    len: u64,
    win_off: u64,
    win: Vec<u8>,
    /// Bytes actually read from the file (reported in the technical details).
    pub bytes_read: u64,
}

const WINDOW: usize = 256 * 1024;

impl<R: Read + Seek> FileSource<R> {
    pub fn new(mut inner: R) -> std::io::Result<FileSource<R>> {
        let len = inner.seek(SeekFrom::End(0))?;
        Ok(FileSource {
            inner,
            len,
            win_off: 0,
            win: Vec::new(),
            bytes_read: 0,
        })
    }

    fn fill(&mut self, offset: u64) -> std::io::Result<()> {
        self.inner.seek(SeekFrom::Start(offset))?;
        let want = (WINDOW as u64).min(self.len.saturating_sub(offset)) as usize;
        self.win.clear();
        self.win.resize(want, 0);
        let mut done = 0;
        while done < want {
            let k = self.inner.read(&mut self.win[done..])?;
            if k == 0 {
                break;
            }
            done += k;
        }
        self.win.truncate(done);
        self.win_off = offset;
        self.bytes_read += done as u64;
        Ok(())
    }
}

impl<R: Read + Seek> Source for FileSource<R> {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        // Large reads bypass the window.
        if buf.len() > WINDOW {
            self.inner.seek(SeekFrom::Start(offset))?;
            let k = self.inner.read(buf)?;
            self.bytes_read += k as u64;
            return Ok(k);
        }
        let in_win = offset >= self.win_off
            && offset + buf.len() as u64 <= self.win_off + self.win.len() as u64;
        if !in_win {
            self.fill(offset)?;
        }
        let start = (offset - self.win_off) as usize;
        let n = buf.len().min(self.win.len().saturating_sub(start));
        buf[..n].copy_from_slice(&self.win[start..start + n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bounded_by_the_real_length_and_the_cap() {
        let data = [1u8, 2, 3, 4, 5];
        let mut s = SliceSource::new(&data);
        assert_eq!(s.read_vec(1, 3, 10), Some(vec![2, 3, 4]));
        assert_eq!(s.read_vec(3, 3, 10), None, "past the end");
        assert_eq!(s.read_vec(0, 5, 4), None, "over the cap");
        assert_eq!(s.read_vec(u64::MAX, 2, 10), None, "overflow");
        assert_eq!(s.read_upto(3, 100, 100), vec![4, 5]);
        let mut f = FileSource::new(std::io::Cursor::new(data.to_vec())).unwrap();
        assert_eq!(f.len(), 5);
        assert_eq!(f.read_vec(0, 5, 5), Some(data.to_vec()));
        assert_eq!(read_array::<2>(&mut f, 4), None);
    }
}
