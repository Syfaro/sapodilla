use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use packed_struct::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{instrument, trace};

const WRAPPER: u8 = 0x7E;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("reader error: {0}")]
    Reader(std::io::Error),
    #[error("invalid data for field: {0}")]
    InvalidData(&'static str),
}

#[derive(Debug, Clone, Serialize)]
pub struct AvocadoPacket {
    pub version: u8,
    pub content_type: ContentType,
    pub interaction_type: InteractionType,
    pub encoding_type: EncodingType,
    pub encryption_mode: EncryptionMode,
    pub terminal_id: u32,
    pub msg_number: u32,
    pub msg_package_total: u16,
    pub msg_package_num: u16,
    pub is_subpackage: bool,
    pub data: Vec<u8>,
}

impl AvocadoPacket {
    pub fn as_json<T>(&self) -> Option<T>
    where
        T: serde::de::DeserializeOwned,
    {
        if self.content_type == ContentType::Message
            && self.encryption_mode == EncryptionMode::None
            && self.encoding_type == EncodingType::Json
        {
            serde_json::from_slice(&self.data).ok()
        } else {
            None
        }
    }
}

#[derive(PackedStruct, Debug, PartialEq)]
#[packed_struct(bit_numbering = "msb0", size_bytes = "2")]
struct AvocadoFlags {
    #[packed_field(bits = "0..=9", endian = "lsb")]
    length: u16,
    #[packed_field(bits = "10")]
    is_subpackage: bool,
    #[packed_field(bits = "11..=13", ty = "enum")]
    encryption_mode: EncryptionMode,
}

impl AvocadoPacket {
    #[instrument(skip_all)]
    pub fn read_one<R>(reader: &mut R) -> Result<Self, ProtocolError>
    where
        R: std::io::Read,
    {
        let prefix = reader.read_u8().map_err(ProtocolError::Reader)?;
        if prefix != WRAPPER {
            return Err(ProtocolError::InvalidData("prefix"));
        }

        let version = reader.read_u8().map_err(ProtocolError::Reader)?;
        let _reserved = reader.read_u8().map_err(ProtocolError::Reader)?;

        let content_type = Self::read_enum(reader, "content_type")?;
        trace!(?content_type);
        let interaction_type = Self::read_enum(reader, "interaction_type")?;
        trace!(?interaction_type);
        let encoding_type = Self::read_enum(reader, "encoding_type")?;
        trace!(?encoding_type);

        let terminal_id = reader
            .read_u32::<LittleEndian>()
            .map_err(ProtocolError::Reader)?;
        trace!(terminal_id);
        let msg_number = reader
            .read_u32::<LittleEndian>()
            .map_err(ProtocolError::Reader)?;
        trace!(msg_number);
        let msg_package_total = reader
            .read_u16::<LittleEndian>()
            .map_err(ProtocolError::Reader)?;
        trace!(msg_package_total);
        let msg_package_num = reader
            .read_u16::<LittleEndian>()
            .map_err(ProtocolError::Reader)?;
        trace!(msg_package_num);

        let flags = reader
            .read_u16::<LittleEndian>()
            .map_err(ProtocolError::Reader)?;
        trace!("flags: {flags:016b}");

        let flags = AvocadoFlags::unpack_from_slice(&flags.to_le_bytes())
            .map_err(|_| ProtocolError::InvalidData("flags"))?;

        let mut data = vec![0u8; usize::from(flags.length)];
        reader
            .read_exact(&mut data)
            .map_err(ProtocolError::Reader)?;
        trace!("data: {}", hex::encode(&data));

        let _checksum = reader.read_u8().map_err(ProtocolError::Reader)?;

        let suffix = reader.read_u8().map_err(ProtocolError::Reader)?;
        if suffix != WRAPPER {
            return Err(ProtocolError::InvalidData("suffix"));
        }

        Ok(Self {
            version,
            content_type,
            interaction_type,
            encoding_type,
            encryption_mode: flags.encryption_mode,
            terminal_id,
            msg_number,
            msg_package_total,
            msg_package_num,
            is_subpackage: flags.is_subpackage,
            data,
        })
    }

    #[instrument(skip_all)]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.data.len() + 22);

        buf.push(WRAPPER);
        buf.push(100); // version
        buf.push(0); // reserved
        buf.push(self.content_type.to_primitive());
        buf.push(self.interaction_type.to_primitive());
        buf.push(self.encoding_type.to_primitive());
        buf.write_u32::<LittleEndian>(self.terminal_id).unwrap();
        buf.write_u32::<LittleEndian>(self.msg_number).unwrap();
        buf.write_u16::<LittleEndian>(self.msg_package_total)
            .unwrap();
        buf.write_u16::<LittleEndian>(self.msg_package_num).unwrap();
        let mut flags = 0u16;
        if self.is_subpackage {
            flags |= 1 << 13
        }
        flags |= u16::from(self.encryption_mode.to_primitive()) << 10;
        flags |= self.data.len() as u16 & 0b00000011_11111111;
        buf.write_u16::<LittleEndian>(flags).unwrap();
        buf.extend_from_slice(&self.data);
        buf.push(Self::checksum(&buf[1..]));
        buf.push(WRAPPER);

        buf
    }

    fn checksum(data: &[u8]) -> u8 {
        data.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
    }

    fn read_enum<R, E>(reader: &mut R, name: &'static str) -> Result<E, ProtocolError>
    where
        R: std::io::Read,
        E: PrimitiveEnum<Primitive = u8>,
    {
        PrimitiveEnum::from_primitive(reader.read_u8().map_err(ProtocolError::Reader)?)
            .ok_or(ProtocolError::InvalidData(name))
    }
}

pub struct AvocadoPacketReader<R> {
    reader: R,
}

impl<R> AvocadoPacketReader<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }
}

impl<R> Iterator for AvocadoPacketReader<R>
where
    R: std::io::Read,
{
    type Item = Result<AvocadoPacket, ProtocolError>;

    fn next(&mut self) -> Option<Self::Item> {
        match AvocadoPacket::read_one(&mut self.reader) {
            Ok(packet) => Some(Ok(packet)),
            Err(ProtocolError::Reader(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                None
            }
            Err(err) => Some(Err(err)),
        }
    }
}

#[derive(PrimitiveEnum_u8, Clone, Copy, Debug, PartialEq, Hash, Serialize)]
pub enum ContentType {
    Message = 1,
    Data = 2,
}

impl std::fmt::Display for ContentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message => write!(f, "message"),
            Self::Data => write!(f, "data"),
        }
    }
}

#[derive(PrimitiveEnum_u8, Clone, Copy, Debug, PartialEq, Hash, Serialize)]
pub enum InteractionType {
    Request = 6,
    Response = 7,
}

impl std::fmt::Display for InteractionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request => write!(f, "request"),
            Self::Response => write!(f, "response"),
        }
    }
}

#[derive(PrimitiveEnum_u8, Clone, Copy, Debug, PartialEq, Hash, Serialize)]
pub enum EncodingType {
    Hexadecimal = 2,
    Json = 3,
}

impl std::fmt::Display for EncodingType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hexadecimal => write!(f, "hexadecimal"),
            Self::Json => write!(f, "json"),
        }
    }
}

#[derive(PrimitiveEnum_u8, Clone, Copy, Debug, PartialEq, Hash, Serialize)]
pub enum EncryptionMode {
    None = 0b000,
    RC4 = 0b010,
}

impl std::fmt::Display for EncryptionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::RC4 => write!(f, "RC4"),
        }
    }
}

#[derive(PrimitiveEnum_u8, Clone, Copy, Debug, PartialEq, Hash, Serialize, Deserialize)]
pub enum JobState {
    Waiting = 1,
    Start = 2,
    Processing = 3,
    ProcessingHeld = 4,
    Pending = 5,
    Terminating = 6,
    Aborted = 7,
    Cancelled = 8,
    Completed = 9,
}

#[derive(PrimitiveEnum_u16, Clone, Copy, Debug, PartialEq, Hash, Serialize, Deserialize)]
pub enum JobSubState {
    WaitingNone = 1000,
    StartNone = 2000,
    ProcessingNone = 3000,
    ProcessingPrintingDataDownloading = 3001,
    ProcessingPrintingDataUploading = 3002,
    ProcessingPrintingDataCloudRendering = 3003,
    ProcessingPrintingDataLocalRendering = 3004,
    ProcessingPrinting = 3005,
    ProcessingHeldNone = 4000,
    PendingNone = 5000,
    TerminatingNone = 6000,
    AbortedNone = 7000,
    CancelledNone = 8000,
    CompletedNone = 9000,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const JSON_REQUEST_DATA: &[u8] = &[
        0x7E, 0x64, 0x00, 0x01, 0x06, 0x03, 0x74, 0x02, 0x00, 0x00, 0x74, 0x02, 0x00, 0x00, 0x01,
        0x00, 0x01, 0x00, 0x69, 0x00, 0x7B, 0x0A, 0x20, 0x20, 0x22, 0x69, 0x64, 0x22, 0x20, 0x3A,
        0x20, 0x36, 0x32, 0x38, 0x2C, 0x0A, 0x20, 0x20, 0x22, 0x6D, 0x65, 0x74, 0x68, 0x6F, 0x64,
        0x22, 0x20, 0x3A, 0x20, 0x22, 0x67, 0x65, 0x74, 0x2D, 0x70, 0x72, 0x6F, 0x70, 0x22, 0x2C,
        0x0A, 0x20, 0x20, 0x22, 0x70, 0x61, 0x72, 0x61, 0x6D, 0x73, 0x22, 0x20, 0x3A, 0x20, 0x5B,
        0x0A, 0x20, 0x20, 0x20, 0x20, 0x22, 0x66, 0x69, 0x72, 0x6D, 0x77, 0x61, 0x72, 0x65, 0x2D,
        0x72, 0x65, 0x76, 0x69, 0x73, 0x69, 0x6F, 0x6E, 0x22, 0x2C, 0x0A, 0x20, 0x20, 0x20, 0x20,
        0x22, 0x62, 0x74, 0x2D, 0x70, 0x68, 0x6F, 0x6E, 0x65, 0x2D, 0x6D, 0x61, 0x63, 0x22, 0x0A,
        0x20, 0x20, 0x5D, 0x0A, 0x7D, 0x59, 0x7E,
    ];

    #[test]
    fn test_read_one() {
        let mut cursor = Cursor::new(JSON_REQUEST_DATA);

        let packet = AvocadoPacket::read_one(&mut cursor);
        assert!(packet.is_ok());
    }

    #[test]
    fn test_flags() {
        let flags = AvocadoFlags {
            is_subpackage: false,
            encryption_mode: EncryptionMode::None,
            length: 1,
        }
        .pack()
        .unwrap();
        assert_eq!(flags, [0b00000001, 0b00000000]);

        let flags = AvocadoFlags {
            is_subpackage: false,
            encryption_mode: EncryptionMode::RC4,
            length: 1,
        }
        .pack()
        .unwrap();
        println!("{:08b} {:08b}", flags[0], flags[1]);
        assert_eq!(flags, [0b00000001, 0b00001000]);

        let flags = AvocadoFlags {
            is_subpackage: true,
            encryption_mode: EncryptionMode::RC4,
            length: 1,
        }
        .pack()
        .unwrap();
        println!("{:08b} {:08b}", flags[0], flags[1]);
        assert_eq!(flags, [0b00000001, 0b00101000]);

        let flags = AvocadoFlags {
            is_subpackage: true,
            encryption_mode: EncryptionMode::RC4,
            length: 255,
        }
        .pack()
        .unwrap();
        println!("{:08b} {:08b}", flags[0], flags[1]);
        assert_eq!(flags, [0b11111111, 0b00101000]);

        let flags = AvocadoFlags {
            is_subpackage: true,
            encryption_mode: EncryptionMode::RC4,
            length: 1023,
        }
        .pack()
        .unwrap();
        println!("{:08b} {:08b}", flags[0], flags[1]);
        assert_eq!(flags, [0b11111111, 0b11101000]);
    }

    #[test]
    fn test_encode() {
        let packet = AvocadoPacket {
            version: 100,
            content_type: ContentType::Message,
            interaction_type: InteractionType::Request,
            encoding_type: EncodingType::Json,
            encryption_mode: EncryptionMode::None,
            terminal_id: 628,
            msg_number: 628,
            msg_package_total: 1,
            msg_package_num: 1,
            is_subpackage: false,
            data: vec![
                0x7B, 0x0A, 0x20, 0x20, 0x22, 0x69, 0x64, 0x22, 0x20, 0x3A, 0x20, 0x36, 0x32, 0x38,
                0x2C, 0x0A, 0x20, 0x20, 0x22, 0x6D, 0x65, 0x74, 0x68, 0x6F, 0x64, 0x22, 0x20, 0x3A,
                0x20, 0x22, 0x67, 0x65, 0x74, 0x2D, 0x70, 0x72, 0x6F, 0x70, 0x22, 0x2C, 0x0A, 0x20,
                0x20, 0x22, 0x70, 0x61, 0x72, 0x61, 0x6D, 0x73, 0x22, 0x20, 0x3A, 0x20, 0x5B, 0x0A,
                0x20, 0x20, 0x20, 0x20, 0x22, 0x66, 0x69, 0x72, 0x6D, 0x77, 0x61, 0x72, 0x65, 0x2D,
                0x72, 0x65, 0x76, 0x69, 0x73, 0x69, 0x6F, 0x6E, 0x22, 0x2C, 0x0A, 0x20, 0x20, 0x20,
                0x20, 0x22, 0x62, 0x74, 0x2D, 0x70, 0x68, 0x6F, 0x6E, 0x65, 0x2D, 0x6D, 0x61, 0x63,
                0x22, 0x0A, 0x20, 0x20, 0x5D, 0x0A, 0x7D,
            ],
        };
        assert_eq!(
            packet.encode(),
            [
                0x7E, 0x64, 0x00, 0x01, 0x06, 0x03, 0x74, 0x02, 0x00, 0x00, 0x74, 0x02, 0x00, 0x00,
                0x01, 0x00, 0x01, 0x00, 0x69, 0x00, 0x7B, 0x0A, 0x20, 0x20, 0x22, 0x69, 0x64, 0x22,
                0x20, 0x3A, 0x20, 0x36, 0x32, 0x38, 0x2C, 0x0A, 0x20, 0x20, 0x22, 0x6D, 0x65, 0x74,
                0x68, 0x6F, 0x64, 0x22, 0x20, 0x3A, 0x20, 0x22, 0x67, 0x65, 0x74, 0x2D, 0x70, 0x72,
                0x6F, 0x70, 0x22, 0x2C, 0x0A, 0x20, 0x20, 0x22, 0x70, 0x61, 0x72, 0x61, 0x6D, 0x73,
                0x22, 0x20, 0x3A, 0x20, 0x5B, 0x0A, 0x20, 0x20, 0x20, 0x20, 0x22, 0x66, 0x69, 0x72,
                0x6D, 0x77, 0x61, 0x72, 0x65, 0x2D, 0x72, 0x65, 0x76, 0x69, 0x73, 0x69, 0x6F, 0x6E,
                0x22, 0x2C, 0x0A, 0x20, 0x20, 0x20, 0x20, 0x22, 0x62, 0x74, 0x2D, 0x70, 0x68, 0x6F,
                0x6E, 0x65, 0x2D, 0x6D, 0x61, 0x63, 0x22, 0x0A, 0x20, 0x20, 0x5D, 0x0A, 0x7D, 0x59,
                0x7E,
            ]
        );
    }
}
