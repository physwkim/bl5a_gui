//! The SPEC server protocol (`SPECD/include/spec_server.h`, the `server`
//! help page): a `svr_head` header and an optional data section per packet.
//!
//! The header has three layouts. Version 2 (124 bytes) has neither `err`
//! nor `flags`, version 3 (128) adds `err`, version 4 (132) adds `flags`.
//! A reader takes the byte order from the magic of each packet and the
//! layout from `vers`/`size`; a later version is read as version 4 with the
//! extra header bytes skipped. A writer uses one [`Framing`]: a client its
//! native order at version 4, the server the order and version its peer
//! sent, since the SPEC server answers each client in the client's own byte
//! order.
//!
//! Numbers travel as `%.15g` strings, strings with their NUL counted in
//! `len`, and an associative array as `index\0value\0...` plus one more NUL.

use std::borrow::Cow;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use epics_libcom_rs::cfmt::CFloat;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// `SV_SPEC_MAGIC`.
pub const MAGIC: u32 = 4_277_009_102;
/// `SV_NAME_LEN`: the property name field, NUL included.
pub const NAME_LEN: usize = 80;
/// `SV_PORT_FIRST`..=`SV_PORT_LAST`.
pub const PORT_FIRST: u16 = 6510;
pub const PORT_LAST: u16 = 6530;
/// `SV_DELETED` (spec_server.h), set in `flags` when a watched variable
/// is deleted.
pub const FLAG_DELETED: i32 = 0x0001;

/// Largest data section accepted; a longer `len` means the stream is lost.
const MAX_DATA: u32 = 1 << 28;
/// Largest header accepted from a later protocol version.
const MAX_HEADER: u32 = 4096;
/// magic, vers, size: what must be read before the layout is known.
const PREAMBLE: usize = 12;

macro_rules! codes {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $code:literal,)* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant = $code,)* }

        impl $name {
            pub fn from_code(code: i32) -> Option<$name> {
                match code {
                    $($code => Some($name::$variant),)*
                    _ => None,
                }
            }

            pub fn code(self) -> i32 {
                self as i32
            }
        }
    };
}

codes! {
    /// `svr_head.cmd`.
    Cmd {
        Close = 1,
        Abort = 2,
        Cmd = 3,
        CmdWithReturn = 4,
        Return = 5,
        Register = 6,
        Unregister = 7,
        Event = 8,
        Func = 9,
        FuncWithReturn = 10,
        ChanRead = 11,
        ChanSend = 12,
        Reply = 13,
        Hello = 14,
        HelloReply = 15,
    }
}

codes! {
    /// `svr_head.type`.
    DataType {
        Double = 1,
        String = 2,
        Error = 3,
        Assoc = 4,
        ArrDouble = 5,
        ArrFloat = 6,
        ArrLong = 7,
        ArrUlong = 8,
        ArrShort = 9,
        ArrUshort = 10,
        ArrChar = 11,
        ArrUchar = 12,
        ArrString = 13,
        ArrLong64 = 14,
        ArrUlong64 = 15,
    }
}

impl DataType {
    /// Bytes per element of a binary type, whose data is swapped between
    /// the sender's byte order and the native one.
    fn binary_width(self) -> Option<usize> {
        match self {
            DataType::Double | DataType::ArrDouble | DataType::ArrLong64 | DataType::ArrUlong64 => {
                Some(8)
            }
            DataType::ArrFloat | DataType::ArrLong | DataType::ArrUlong => Some(4),
            DataType::ArrShort | DataType::ArrUshort => Some(2),
            _ => None,
        }
    }
}

/// Reverses each element of binary data of type `t` sent in `endian`.
fn to_native(data: &mut [u8], t: Option<DataType>, endian: Endian) {
    if endian == Endian::NATIVE {
        return;
    }
    if let Some(width) = t.and_then(DataType::binary_width) {
        data.chunks_exact_mut(width).for_each(<[u8]>::reverse);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    pub const NATIVE: Endian = if cfg!(target_endian = "big") {
        Endian::Big
    } else {
        Endian::Little
    };

    fn get_u32(self, b: &[u8], at: usize) -> u32 {
        let w = [b[at], b[at + 1], b[at + 2], b[at + 3]];
        match self {
            Endian::Little => u32::from_le_bytes(w),
            Endian::Big => u32::from_be_bytes(w),
        }
    }

    fn put_u32(self, out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        });
    }
}

/// The header layout, by `svr_head.vers`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    V2 = 2,
    V3 = 3,
    V4 = 4,
}

impl Version {
    fn from_vers(vers: u32) -> Version {
        match vers {
            0..=2 => Version::V2,
            3 => Version::V3,
            _ => Version::V4,
        }
    }

    /// Offset of `name`; `err` and `flags` sit between `len` and it.
    fn name_offset(self) -> usize {
        match self {
            Version::V2 => 44,
            Version::V3 => 48,
            Version::V4 => 52,
        }
    }

    pub fn header_size(self) -> usize {
        self.name_offset() + NAME_LEN
    }
}

/// Byte order and header layout of one side of a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framing {
    pub endian: Endian,
    pub version: Version,
}

impl Framing {
    /// What a client sends: its own byte order, the current header.
    pub const CLIENT: Framing = Framing {
        endian: Endian::NATIVE,
        version: Version::V4,
    };
}

/// `svr_head` without the fields the framing and the data determine
/// (`magic`, `vers`, `size`, `len`).
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub sn: u32,
    pub sec: u32,
    pub usec: u32,
    pub cmd: Cmd,
    /// `None` is type code 0, sent with packets that carry no data.
    pub data_type: Option<DataType>,
    pub rows: u32,
    pub cols: u32,
    pub err: i32,
    pub flags: i32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub header: Header,
    /// Binary types (`SV_DOUBLE`, `SV_ARR_*`) hold native-order elements;
    /// the reader and the encoder swap them for a foreign peer.
    pub data: Vec<u8>,
}

impl Packet {
    /// A packet without data, stamped with the current time.
    pub fn new(cmd: Cmd, sn: u32, name: impl Into<String>) -> Packet {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Packet {
            header: Header {
                sn,
                sec: now.as_secs() as u32,
                usec: now.subsec_micros(),
                cmd,
                data_type: None,
                rows: 0,
                cols: 0,
                err: 0,
                flags: 0,
                name: name.into(),
            },
            data: Vec::new(),
        }
    }

    /// The reply to `request`: its serial number and property name.
    pub fn reply_to(request: &Header) -> Packet {
        Packet::new(Cmd::Reply, request.sn, request.name.clone())
    }

    /// An `SV_STRING` data section.
    pub fn with_string(mut self, s: &str) -> Packet {
        self.header.data_type = Some(DataType::String);
        self.data = nul_terminated(s);
        self
    }

    /// A number, as the server sends one: an `SV_STRING` of `%.15g`.
    pub fn with_number(self, x: f64) -> Packet {
        self.with_string(&format_number(x))
    }

    /// An `SV_ERROR` data section.
    pub fn with_error(mut self, message: &str) -> Packet {
        self.header.data_type = Some(DataType::Error);
        self.data = nul_terminated(message);
        self
    }

    /// An `SV_ASSOC` data section.
    pub fn with_assoc<'a>(mut self, pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Packet {
        self.header.data_type = Some(DataType::Assoc);
        let mut data = Vec::new();
        for (index, value) in pairs {
            data.extend(nul_terminated(index));
            data.extend(nul_terminated(value));
        }
        data.push(0);
        self.data = data;
        self
    }

    /// The data as text, up to its first NUL. Clients need not send the
    /// NUL (speca does not), so the whole section is the text without one.
    pub fn text(&self) -> Cow<'_, str> {
        let end = self
            .data
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.data.len());
        String::from_utf8_lossy(&self.data[..end])
    }

    /// The data as a number, from text or from `SV_DOUBLE`.
    pub fn number(&self) -> Option<f64> {
        if self.header.data_type == Some(DataType::Double) {
            return Some(f64::from_ne_bytes(self.data[..].try_into().ok()?));
        }
        parse_number(&self.text())
    }

    /// An `SV_ASSOC` data section as (index, value) pairs.
    pub fn assoc(&self) -> Vec<(String, String)> {
        let mut fields = self.data.split(|&b| b == 0);
        let mut pairs = Vec::new();
        while let (Some(index), Some(value)) = (fields.next(), fields.next()) {
            if index.is_empty() {
                break;
            }
            pairs.push((
                String::from_utf8_lossy(index).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            ));
        }
        pairs
    }
}

fn nul_terminated(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

/// `printf("%.15g")`, the server's number-to-string conversion.
pub fn format_number(x: f64) -> String {
    format!("{:.15}", CFloat(x).g())
}

/// A number sent as text: surrounding white space is ignored.
pub fn parse_number(s: &str) -> Option<f64> {
    s.trim().parse().ok()
}

#[derive(Debug)]
pub enum ReadError {
    /// The connection failed or closed.
    Io(io::Error),
    /// Not a SPEC packet; the stream cannot be resynchronised.
    BadMagic(u32),
    /// A header size the version cannot have.
    BadSize { vers: u32, size: u32 },
    /// A data length beyond any packet SPEC sends.
    TooLong(u32),
    /// A whole packet with a command or type code this crate does not know.
    /// It was consumed, so the next read starts on the next packet.
    Unknown {
        sn: u32,
        cmd: i32,
        data_type: i32,
        name: String,
    },
}

impl ReadError {
    /// Whether the stream is lost: everything except [`ReadError::Unknown`].
    pub fn is_fatal(&self) -> bool {
        !matches!(self, ReadError::Unknown { .. })
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "{e}"),
            ReadError::BadMagic(m) => write!(f, "bad magic {m:#010x}"),
            ReadError::BadSize { vers, size } => {
                write!(f, "header size {size} impossible for version {vers}")
            }
            ReadError::TooLong(len) => write!(f, "data length {len} too long"),
            ReadError::Unknown {
                cmd,
                data_type,
                name,
                ..
            } => write!(f, "unknown cmd {cmd} / type {data_type} for '{name}'"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> ReadError {
        ReadError::Io(e)
    }
}

/// Reads one packet and the framing it was sent with.
pub async fn read_packet<R: AsyncRead + Unpin>(r: &mut R) -> Result<(Packet, Framing), ReadError> {
    let mut head = vec![0u8; PREAMBLE];
    r.read_exact(&mut head).await?;
    let endian = if Endian::Little.get_u32(&head, 0) == MAGIC {
        Endian::Little
    } else if Endian::Big.get_u32(&head, 0) == MAGIC {
        Endian::Big
    } else {
        return Err(ReadError::BadMagic(Endian::NATIVE.get_u32(&head, 0)));
    };
    let vers = endian.get_u32(&head, 4);
    let size = endian.get_u32(&head, 8);
    let version = Version::from_vers(vers);
    if (size as usize) < version.header_size() || size > MAX_HEADER {
        return Err(ReadError::BadSize { vers, size });
    }
    head.resize(size as usize, 0);
    r.read_exact(&mut head[PREAMBLE..]).await?;

    let u = |at| endian.get_u32(&head, at);
    let len = u(40);
    if len > MAX_DATA {
        return Err(ReadError::TooLong(len));
    }
    let mut data = vec![0u8; len as usize];
    r.read_exact(&mut data).await?;

    let (err, flags) = match version {
        Version::V2 => (0, 0),
        Version::V3 => (u(44) as i32, 0),
        Version::V4 => (u(44) as i32, u(48) as i32),
    };
    let field = &head[version.name_offset()..version.header_size()];
    let end = field.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    let name = String::from_utf8_lossy(&field[..end]).into_owned();
    let sn = u(12);
    let (cmd_code, type_code) = (u(24) as i32, u(28) as i32);
    let data_type = match type_code {
        0 => Some(None),
        code => DataType::from_code(code).map(Some),
    };
    let (Some(cmd), Some(data_type)) = (Cmd::from_code(cmd_code), data_type) else {
        return Err(ReadError::Unknown {
            sn,
            cmd: cmd_code,
            data_type: type_code,
            name,
        });
    };
    to_native(&mut data, data_type, endian);
    let header = Header {
        sn,
        sec: u(16),
        usec: u(20),
        cmd,
        data_type,
        rows: u(32),
        cols: u(36),
        err,
        flags,
        name,
    };
    Ok((Packet { header, data }, Framing { endian, version }))
}

/// The bytes of `packet` as `framing` lays them out.
pub fn encode(packet: &Packet, framing: Framing) -> io::Result<Vec<u8>> {
    let h = &packet.header;
    if h.name.len() >= NAME_LEN || h.name.as_bytes().contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("property name '{}' does not fit svr_head.name", h.name),
        ));
    }
    let len = u32::try_from(packet.data.len())
        .ok()
        .filter(|&n| n <= MAX_DATA)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "data section too long"))?;
    let version = framing.version;
    let e = framing.endian;
    let mut out = Vec::with_capacity(version.header_size() + packet.data.len());
    for v in [
        MAGIC,
        version as u32,
        version.header_size() as u32,
        h.sn,
        h.sec,
        h.usec,
        h.cmd.code() as u32,
        h.data_type.map_or(0, DataType::code) as u32,
        h.rows,
        h.cols,
        len,
    ] {
        e.put_u32(&mut out, v);
    }
    if version >= Version::V3 {
        e.put_u32(&mut out, h.err as u32);
    }
    if version >= Version::V4 {
        e.put_u32(&mut out, h.flags as u32);
    }
    out.extend_from_slice(h.name.as_bytes());
    out.resize(version.header_size(), 0);
    let at = out.len();
    out.extend_from_slice(&packet.data);
    // Native to `e` is the same per-element reversal as `e` to native.
    to_native(&mut out[at..], h.data_type, e);
    Ok(out)
}

/// Writes `packet` in one write, so concurrent writers on a shared stream
/// cannot interleave inside it.
pub async fn write_packet<W: AsyncWrite + Unpin>(
    w: &mut W,
    packet: &Packet,
    framing: Framing,
) -> io::Result<()> {
    w.write_all(&encode(packet, framing)?).await
}

#[cfg(test)]
mod tests;
