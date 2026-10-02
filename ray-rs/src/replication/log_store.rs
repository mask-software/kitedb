//! Replication segment log storage.

use super::durability::SidecarSync;
use crate::error::{KiteError, Result};
use crate::util::crc::{crc32, crc32_multi};
use byteorder::{LittleEndian, ReadBytesExt};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const FRAME_MAGIC: u32 = 0x474F_4C52; // "RLOG" in little-endian u32
const FRAME_VERSION: u16 = 1;
const FRAME_FLAG_CRC32_DISABLED: u16 = 0x0001;
const FRAME_HEADER_SIZE: usize = std::mem::size_of::<u32>()
  + std::mem::size_of::<u16>()
  + std::mem::size_of::<u16>()
  + std::mem::size_of::<u64>()
  + std::mem::size_of::<u64>()
  + std::mem::size_of::<u32>()
  + std::mem::size_of::<u32>();
const MAX_FRAME_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationFrame {
  pub epoch: u64,
  pub log_index: u64,
  pub payload: Vec<u8>,
}

impl ReplicationFrame {
  pub fn new(epoch: u64, log_index: u64, payload: Vec<u8>) -> Self {
    Self {
      epoch,
      log_index,
      payload,
    }
  }
}

/// Frames matched by a filtered read, the offset scanning stopped at, and the
/// (epoch, log_index) of the last frame seen (matched or not), if any.
pub type FilteredFrameRead = (Vec<ReplicationFrame>, u64, Option<(u64, u64)>);

#[derive(Debug)]
pub struct SegmentLogStore {
  path: PathBuf,
  file: File,
  write_buffer: Vec<u8>,
  write_chunks: Vec<Vec<u8>>,
  queued_bytes: usize,
  write_buffer_limit: usize,
  writable: bool,
  sync: SidecarSync,
}

impl SegmentLogStore {
  pub fn create(path: impl AsRef<Path>) -> Result<Self> {
    let path = path.as_ref().to_path_buf();

    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }

    let file = OpenOptions::new()
      .create(true)
      .truncate(true)
      .read(true)
      .write(true)
      .open(&path)?;

    Ok(Self {
      path,
      file,
      write_buffer: Vec::new(),
      write_chunks: Vec::new(),
      queued_bytes: 0,
      write_buffer_limit: 0,
      writable: true,
      sync: SidecarSync::default(),
    })
  }

  pub fn open(path: impl AsRef<Path>) -> Result<Self> {
    let path = path.as_ref().to_path_buf();
    let file = OpenOptions::new().read(true).open(&path)?;

    Ok(Self {
      path,
      file,
      write_buffer: Vec::new(),
      write_chunks: Vec::new(),
      queued_bytes: 0,
      write_buffer_limit: 0,
      writable: false,
      sync: SidecarSync::default(),
    })
  }

  pub fn open_or_create_append(path: impl AsRef<Path>) -> Result<Self> {
    Self::open_or_create_append_with_buffer(path, 0)
  }

  pub fn open_or_create_append_with_buffer(
    path: impl AsRef<Path>,
    write_buffer_limit: usize,
  ) -> Result<Self> {
    Self::open_append(path, write_buffer_limit, SidecarSync::default())
  }

  /// Open (or create) a segment for appends that buffers up to
  /// `write_buffer_limit` bytes in memory and makes them durable with `sync`.
  pub fn open_append(
    path: impl AsRef<Path>,
    write_buffer_limit: usize,
    sync: SidecarSync,
  ) -> Result<Self> {
    let path = path.as_ref().to_path_buf();

    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }

    let file = OpenOptions::new()
      .create(true)
      .read(true)
      .append(true)
      .open(&path)?;

    Ok(Self {
      path,
      file,
      write_buffer: Vec::with_capacity(write_buffer_limit),
      write_chunks: Vec::new(),
      queued_bytes: 0,
      write_buffer_limit,
      writable: true,
      sync,
    })
  }

  pub fn append(&mut self, frame: &ReplicationFrame) -> Result<()> {
    self.append_payload_segments_with_crc(
      frame.epoch,
      frame.log_index,
      &[frame.payload.as_slice()],
      true,
    )?;
    Ok(())
  }

  pub fn append_payload_segments(
    &mut self,
    epoch: u64,
    log_index: u64,
    payload_segments: &[&[u8]],
  ) -> Result<u64> {
    self.append_payload_segments_with_crc(epoch, log_index, payload_segments, true)
  }

  pub fn append_payload_segments_with_crc(
    &mut self,
    epoch: u64,
    log_index: u64,
    payload_segments: &[&[u8]],
    with_crc: bool,
  ) -> Result<u64> {
    if !self.writable {
      return Err(KiteError::InvalidReplication(
        "cannot append to read-only segment log store".to_string(),
      ));
    }

    let payload_len = payload_segments.iter().try_fold(0usize, |acc, segment| {
      acc
        .checked_add(segment.len())
        .ok_or_else(|| KiteError::InvalidReplication("frame payload too large".to_string()))
    })?;

    if payload_len > MAX_FRAME_PAYLOAD_BYTES {
      return Err(KiteError::InvalidReplication(format!(
        "frame payload too large: {payload_len} bytes"
      )));
    }

    let payload_len_u32 = u32::try_from(payload_len).map_err(|_| {
      KiteError::InvalidReplication(format!("payload length does not fit u32: {payload_len}"))
    })?;

    let flags = if with_crc {
      0
    } else {
      FRAME_FLAG_CRC32_DISABLED
    };
    let crc32 = if with_crc {
      crc32_multi(payload_segments)
    } else {
      0
    };

    let mut header = [0u8; FRAME_HEADER_SIZE];
    header[0..4].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
    header[4..6].copy_from_slice(&FRAME_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&flags.to_le_bytes());
    header[8..16].copy_from_slice(&epoch.to_le_bytes());
    header[16..24].copy_from_slice(&log_index.to_le_bytes());
    header[24..28].copy_from_slice(&payload_len_u32.to_le_bytes());
    header[28..32].copy_from_slice(&crc32.to_le_bytes());
    if self.write_buffer_limit > 0 {
      self.write_buffer.extend_from_slice(&header);
      for segment in payload_segments {
        self.write_buffer.extend_from_slice(segment);
      }
      if self.write_buffer.len().saturating_add(self.queued_bytes) >= self.write_buffer_limit {
        self.flush()?;
      }
    } else {
      self.file.write_all(&header)?;
      for segment in payload_segments {
        self.file.write_all(segment)?;
      }
    }

    Ok(FRAME_HEADER_SIZE as u64 + payload_len as u64)
  }

  pub fn append_payload_owned_segments_with_crc(
    &mut self,
    epoch: u64,
    log_index: u64,
    mut payload_segments: Vec<Vec<u8>>,
    with_crc: bool,
  ) -> Result<u64> {
    if !self.writable {
      return Err(KiteError::InvalidReplication(
        "cannot append to read-only segment log store".to_string(),
      ));
    }

    let payload_len = payload_segments.iter().try_fold(0usize, |acc, segment| {
      acc
        .checked_add(segment.len())
        .ok_or_else(|| KiteError::InvalidReplication("frame payload too large".to_string()))
    })?;

    if payload_len > MAX_FRAME_PAYLOAD_BYTES {
      return Err(KiteError::InvalidReplication(format!(
        "frame payload too large: {payload_len} bytes"
      )));
    }

    let payload_len_u32 = u32::try_from(payload_len).map_err(|_| {
      KiteError::InvalidReplication(format!("payload length does not fit u32: {payload_len}"))
    })?;

    let flags = if with_crc {
      0
    } else {
      FRAME_FLAG_CRC32_DISABLED
    };
    let crc32 = if with_crc {
      let refs: Vec<&[u8]> = payload_segments
        .iter()
        .map(|segment| segment.as_slice())
        .collect();
      crc32_multi(&refs)
    } else {
      0
    };

    let mut header = [0u8; FRAME_HEADER_SIZE];
    header[0..4].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
    header[4..6].copy_from_slice(&FRAME_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&flags.to_le_bytes());
    header[8..16].copy_from_slice(&epoch.to_le_bytes());
    header[16..24].copy_from_slice(&log_index.to_le_bytes());
    header[24..28].copy_from_slice(&payload_len_u32.to_le_bytes());
    header[28..32].copy_from_slice(&crc32.to_le_bytes());

    if self.write_buffer_limit > 0 {
      self.write_chunks.push(header.to_vec());
      self.queued_bytes = self.queued_bytes.saturating_add(FRAME_HEADER_SIZE);
      for segment in payload_segments.drain(..) {
        self.queued_bytes = self.queued_bytes.saturating_add(segment.len());
        self.write_chunks.push(segment);
      }
      if self.write_buffer.len().saturating_add(self.queued_bytes) >= self.write_buffer_limit {
        self.flush()?;
      }
    } else {
      self.file.write_all(&header)?;
      for segment in payload_segments {
        self.file.write_all(&segment)?;
      }
    }

    Ok(FRAME_HEADER_SIZE as u64 + payload_len as u64)
  }

  /// Whether appended frames are still held in memory, unwritten to the file.
  pub fn has_buffered_frames(&self) -> bool {
    !self.write_buffer.is_empty() || !self.write_chunks.is_empty()
  }

  pub fn file_len(&self) -> Result<u64> {
    let metadata = self.file.metadata()?;
    Ok(
      metadata
        .len()
        .saturating_add(self.write_buffer.len() as u64)
        .saturating_add(self.queued_bytes as u64),
    )
  }

  pub fn flush(&mut self) -> Result<()> {
    if !self.writable {
      return Ok(());
    }

    if self.write_buffer.is_empty() && self.write_chunks.is_empty() {
      return Ok(());
    }

    if !self.write_buffer.is_empty() {
      self.file.write_all(&self.write_buffer)?;
      self.write_buffer.clear();
    }
    for chunk in &self.write_chunks {
      self.file.write_all(chunk)?;
    }
    self.write_chunks.clear();
    self.queued_bytes = 0;
    Ok(())
  }

  pub fn sync(&mut self) -> Result<()> {
    if self.writable {
      self.flush()?;
      self.sync.sync_file(&self.file)?;
    }

    Ok(())
  }

  pub fn read_all(&self) -> Result<Vec<ReplicationFrame>> {
    let file = OpenOptions::new().read(true).open(&self.path)?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();

    while let Some(frame) = read_frame(&mut reader)? {
      frames.push(frame);
    }

    Ok(frames)
  }

  /// Last frame written to the segment file, if any.
  pub fn read_last_frame(&self) -> Result<Option<ReplicationFrame>> {
    let file = OpenOptions::new().read(true).open(&self.path)?;
    let mut reader = BufReader::new(file);
    let mut last = None;

    while let Some(frame) = read_frame(&mut reader)? {
      last = Some(frame);
    }

    Ok(last)
  }

  pub fn read_filtered(
    &self,
    mut include: impl FnMut(&ReplicationFrame) -> bool,
    max_frames: usize,
  ) -> Result<Vec<ReplicationFrame>> {
    let file = OpenOptions::new().read(true).open(&self.path)?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();

    while let Some(frame) = read_frame(&mut reader)? {
      if include(&frame) {
        frames.push(frame);
        if max_frames > 0 && frames.len() >= max_frames {
          break;
        }
      }
    }

    Ok(frames)
  }

  pub fn read_filtered_from_offset(
    &self,
    start_offset: u64,
    mut include: impl FnMut(&ReplicationFrame) -> bool,
    max_frames: usize,
  ) -> Result<FilteredFrameRead> {
    let mut file = OpenOptions::new().read(true).open(&self.path)?;
    let file_len = file.metadata()?.len();
    let clamped_start = start_offset.min(file_len);
    file.seek(SeekFrom::Start(clamped_start))?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();
    let mut last_seen = None;

    while let Some(frame) = read_frame(&mut reader)? {
      last_seen = Some((frame.epoch, frame.log_index));
      if include(&frame) {
        frames.push(frame);
        if max_frames > 0 && frames.len() >= max_frames {
          break;
        }
      }
    }

    let next_offset = reader.stream_position()?;
    Ok((frames, next_offset, last_seen))
  }
}

impl Drop for SegmentLogStore {
  fn drop(&mut self) {
    let _ = self.flush();
  }
}

fn read_frame(reader: &mut impl Read) -> Result<Option<ReplicationFrame>> {
  let magic = match reader.read_u32::<LittleEndian>() {
    Ok(value) => value,
    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
    Err(error) => return Err(KiteError::Io(error)),
  };

  if magic != FRAME_MAGIC {
    return Err(KiteError::InvalidWal(format!(
      "invalid replication frame magic: 0x{magic:08X}"
    )));
  }

  let version = read_u16_checked(reader, "version")?;
  let flags = read_u16_checked(reader, "reserved")?;
  let epoch = read_u64_checked(reader, "epoch")?;
  let log_index = read_u64_checked(reader, "log_index")?;
  let payload_len = read_u32_checked(reader, "payload_len")?;
  let stored_crc32 = read_u32_checked(reader, "payload_crc32")?;

  if version != FRAME_VERSION {
    return Err(KiteError::VersionMismatch {
      required: version as u32,
      current: FRAME_VERSION as u32,
    });
  }

  if flags & !FRAME_FLAG_CRC32_DISABLED != 0 {
    return Err(KiteError::InvalidWal(format!(
      "unsupported replication frame flags: 0x{flags:04X}"
    )));
  }

  let crc_disabled = (flags & FRAME_FLAG_CRC32_DISABLED) != 0;
  let payload_len = payload_len as usize;
  if payload_len > MAX_FRAME_PAYLOAD_BYTES {
    return Err(KiteError::InvalidWal(format!(
      "frame payload exceeds limit: {payload_len}"
    )));
  }

  let mut payload = vec![0; payload_len];
  reader
    .read_exact(&mut payload)
    .map_err(|error| map_unexpected_eof(error, "payload"))?;

  if !crc_disabled {
    let computed_crc32 = crc32(&payload);
    if computed_crc32 != stored_crc32 {
      return Err(KiteError::CrcMismatch {
        stored: stored_crc32,
        computed: computed_crc32,
      });
    }
  }

  Ok(Some(ReplicationFrame::new(epoch, log_index, payload)))
}

fn read_u16_checked(reader: &mut impl Read, field: &'static str) -> Result<u16> {
  reader
    .read_u16::<LittleEndian>()
    .map_err(|error| map_unexpected_eof(error, field))
}

fn read_u32_checked(reader: &mut impl Read, field: &'static str) -> Result<u32> {
  reader
    .read_u32::<LittleEndian>()
    .map_err(|error| map_unexpected_eof(error, field))
}

fn read_u64_checked(reader: &mut impl Read, field: &'static str) -> Result<u64> {
  reader
    .read_u64::<LittleEndian>()
    .map_err(|error| map_unexpected_eof(error, field))
}

fn map_unexpected_eof(error: io::Error, field: &'static str) -> KiteError {
  if error.kind() == io::ErrorKind::UnexpectedEof {
    KiteError::InvalidWal(format!(
      "truncated replication segment while reading {field}"
    ))
  } else {
    KiteError::Io(error)
  }
}

#[cfg(test)]
mod tests {
  use super::{ReplicationFrame, SegmentLogStore, FRAME_HEADER_SIZE};

  #[test]
  fn append_then_scan_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("segment.rlog");

    let mut store = SegmentLogStore::create(&path).expect("create");
    store
      .append(&ReplicationFrame::new(1, 1, b"hello".to_vec()))
      .expect("append");
    store
      .append(&ReplicationFrame::new(1, 2, b"world".to_vec()))
      .expect("append");
    store.sync().expect("sync");

    let reader = SegmentLogStore::open(&path).expect("open");
    let frames = reader.read_all().expect("read");

    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].payload, b"hello");
    assert_eq!(frames[1].payload, b"world");
  }

  #[test]
  fn append_payload_segments_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("segment-segmented.rlog");

    let mut store = SegmentLogStore::create(&path).expect("create");
    store
      .append_payload_segments(3, 9, &[b"hello", b"-", b"world"])
      .expect("append");
    store.sync().expect("sync");

    let reader = SegmentLogStore::open(&path).expect("open");
    let frames = reader.read_all().expect("read");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].epoch, 3);
    assert_eq!(frames[0].log_index, 9);
    assert_eq!(frames[0].payload, b"hello-world");
  }

  #[test]
  fn truncated_frame_header_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("segment.rlog");

    let mut store = SegmentLogStore::create(&path).expect("create");
    store
      .append(&ReplicationFrame::new(1, 1, b"abc".to_vec()))
      .expect("append");
    store.sync().expect("sync");

    let mut bytes = std::fs::read(&path).expect("read bytes");
    bytes.truncate(FRAME_HEADER_SIZE - 1);
    std::fs::write(&path, bytes).expect("write truncated");

    let reader = SegmentLogStore::open(&path).expect("open");
    assert!(reader.read_all().is_err());
  }
}
