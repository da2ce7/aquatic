use std::io::{self, Cursor, Write};

use byteorder::{NetworkEndian, WriteBytesExt};
use either::Either;
use zerocopy::FromZeroes;
use zerocopy::{byteorder::network_endian::I32, AsBytes, FromBytes};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use aquatic_peer_id::PeerId;

use super::common::*;

const PROTOCOL_IDENTIFIER: i64 = 4_497_486_125_440;

#[derive(PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Request {
    Connect(ConnectRequest),
    Announce(AnnounceRequest),
    Scrape(ScrapeRequest),
}

impl Request {
    pub fn write_bytes(self, bytes: &mut impl Write) -> Result<(), io::Error> {
        match self {
            Request::Connect(r) => r.write_bytes(bytes),
            Request::Announce(r) => r.write_bytes(bytes),
            Request::Scrape(r) => r.write_bytes(bytes),
        }
    }

    pub fn parse_bytes(bytes: &[u8], max_scrape_torrents: u8) -> Result<Self, RequestParseError> {
        let action = bytes
            .get(8..12)
            .map(|bytes| I32::from_bytes(bytes.try_into().unwrap()))
            .ok_or_else(|| RequestParseError::unsendable_text("Couldn't parse action"))?;

        match action.get() {
            // Connect
            0 => {
                let mut bytes = Cursor::new(bytes);

                let protocol_identifier =
                    read_i64_ne(&mut bytes).map_err(RequestParseError::unsendable_io)?;
                let _action = read_i32_ne(&mut bytes).map_err(RequestParseError::unsendable_io)?;
                let transaction_id = read_i32_ne(&mut bytes)
                    .map(TransactionId)
                    .map_err(RequestParseError::unsendable_io)?;

                if protocol_identifier.get() == PROTOCOL_IDENTIFIER {
                    Ok((ConnectRequest { transaction_id }).into())
                } else {
                    Err(RequestParseError::unsendable_text(
                        "Protocol identifier missing",
                    ))
                }
            }
            // Announce
            1 => {
                let request = AnnounceRequest::read_from_prefix(bytes)
                    .ok_or_else(|| RequestParseError::unsendable_text("invalid data"))?;

                // Make sure not to create AnnounceEventBytes with invalid value
                if matches!(request.event.0.get(), (0..=3)) {
                    Ok(Request::Announce(request))
                } else {
                    Err(RequestParseError::sendable_text(
                        "Invalid announce event",
                        request.connection_id,
                        request.transaction_id,
                    ))
                }
            }
            // Scrape
            2 => {
                let mut bytes = Cursor::new(bytes);

                let connection_id = read_i64_ne(&mut bytes)
                    .map(ConnectionId)
                    .map_err(RequestParseError::unsendable_io)?;
                let _action = read_i32_ne(&mut bytes).map_err(RequestParseError::unsendable_io)?;
                let transaction_id = read_i32_ne(&mut bytes)
                    .map(TransactionId)
                    .map_err(RequestParseError::unsendable_io)?;

                let remaining_bytes = {
                    let position = bytes.position() as usize;
                    let inner = bytes.into_inner();
                    &inner[position..]
                };

                let info_hashes = FromBytes::slice_from(remaining_bytes).ok_or_else(|| {
                    RequestParseError::sendable_text(
                        "Invalid info hash list. Note that full scrapes are not allowed",
                        connection_id,
                        transaction_id,
                    )
                })?;

                let info_hashes = Vec::from(
                    &info_hashes[..(max_scrape_torrents as usize).min(info_hashes.len())],
                );

                Ok((ScrapeRequest {
                    connection_id,
                    transaction_id,
                    info_hashes,
                })
                .into())
            }

            _ => Err(RequestParseError::unsendable_text("Invalid action")),
        }
    }
}

impl From<ConnectRequest> for Request {
    fn from(r: ConnectRequest) -> Self {
        Self::Connect(r)
    }
}

impl From<AnnounceRequest> for Request {
    fn from(r: AnnounceRequest) -> Self {
        Self::Announce(r)
    }
}

impl From<ScrapeRequest> for Request {
    fn from(r: ScrapeRequest) -> Self {
        Self::Scrape(r)
    }
}

#[derive(PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConnectRequest {
    pub transaction_id: TransactionId,
}

impl ConnectRequest {
    pub fn write_bytes(&self, bytes: &mut impl Write) -> Result<(), io::Error> {
        bytes.write_i64::<NetworkEndian>(PROTOCOL_IDENTIFIER)?;
        bytes.write_i32::<NetworkEndian>(0)?;
        bytes.write_all(self.transaction_id.as_bytes())?;

        Ok(())
    }
}

#[derive(PartialEq, Eq, Clone, Debug, AsBytes, FromBytes, FromZeroes)]
#[repr(C, packed)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct AnnounceRequest {
    pub connection_id: ConnectionId,
    /// This field is only present to enable zero-copy serialization and
    /// deserialization.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub action_placeholder: AnnounceActionPlaceholder,
    pub transaction_id: TransactionId,
    pub info_hash: InfoHash,
    pub peer_id: PeerId,
    pub bytes_downloaded: NumberOfBytes,
    pub bytes_left: NumberOfBytes,
    pub bytes_uploaded: NumberOfBytes,
    pub event: AnnounceEventBytes,
    pub ip_address: Ipv4AddrBytes,
    pub key: PeerKey,
    pub peers_wanted: NumberOfPeers,
    pub port: Port,
}

impl AnnounceRequest {
    pub fn write_bytes(&self, bytes: &mut impl Write) -> Result<(), io::Error> {
        bytes.write_all(self.as_bytes())
    }
}

/// Note: Request::from_bytes only creates this struct with value 1
#[derive(PartialEq, Eq, Clone, Copy, Debug, AsBytes, FromBytes, FromZeroes)]
#[repr(transparent)]
pub struct AnnounceActionPlaceholder(I32);

impl Default for AnnounceActionPlaceholder {
    fn default() -> Self {
        Self(I32::new(1))
    }
}

/// Note: Request::from_bytes only creates this struct with values 0..=3
#[derive(PartialEq, Eq, Clone, Copy, Debug, AsBytes, FromBytes, FromZeroes)]
#[repr(transparent)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(from = "AnnounceEvent", into = "AnnounceEvent")
)]
pub struct AnnounceEventBytes(I32);

impl From<AnnounceEvent> for AnnounceEventBytes {
    fn from(value: AnnounceEvent) -> Self {
        Self(I32::new(value as i32))
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(rename_all = "lowercase")
)]
pub enum AnnounceEvent {
    None,
    Completed,
    Started,
    Stopped,
}

impl From<AnnounceEventBytes> for AnnounceEvent {
    fn from(value: AnnounceEventBytes) -> Self {
        match value.0.get() {
            1 => Self::Completed,
            2 => Self::Started,
            3 => Self::Stopped,
            _ => Self::None,
        }
    }
}

#[derive(PartialEq, Eq, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ScrapeRequest {
    pub connection_id: ConnectionId,
    pub transaction_id: TransactionId,
    pub info_hashes: Vec<InfoHash>,
}

impl ScrapeRequest {
    pub fn write_bytes(&self, bytes: &mut impl Write) -> Result<(), io::Error> {
        bytes.write_all(self.connection_id.as_bytes())?;
        bytes.write_i32::<NetworkEndian>(2)?;
        bytes.write_all(self.transaction_id.as_bytes())?;
        bytes.write_all((*self.info_hashes.as_slice()).as_bytes())?;

        Ok(())
    }
}

#[derive(Debug)]
pub enum RequestParseError {
    Sendable {
        connection_id: ConnectionId,
        transaction_id: TransactionId,
        err: &'static str,
    },
    Unsendable {
        err: Either<io::Error, &'static str>,
    },
}

impl RequestParseError {
    pub fn sendable_text(
        text: &'static str,
        connection_id: ConnectionId,
        transaction_id: TransactionId,
    ) -> Self {
        Self::Sendable {
            connection_id,
            transaction_id,
            err: text,
        }
    }
    pub fn unsendable_io(err: io::Error) -> Self {
        Self::Unsendable {
            err: Either::Left(err),
        }
    }
    pub fn unsendable_text(text: &'static str) -> Self {
        Self::Unsendable {
            err: Either::Right(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use quickcheck::TestResult;
    use quickcheck_macros::quickcheck;
    use zerocopy::network_endian::{I32, I64, U16};

    use super::*;

    impl quickcheck::Arbitrary for AnnounceEvent {
        fn arbitrary(g: &mut quickcheck::Gen) -> Self {
            match (bool::arbitrary(g), bool::arbitrary(g)) {
                (false, false) => Self::Started,
                (true, false) => Self::Started,
                (false, true) => Self::Completed,
                (true, true) => Self::None,
            }
        }
    }

    impl quickcheck::Arbitrary for ConnectRequest {
        fn arbitrary(g: &mut quickcheck::Gen) -> Self {
            Self {
                transaction_id: TransactionId(I32::new(i32::arbitrary(g))),
            }
        }
    }

    impl quickcheck::Arbitrary for AnnounceRequest {
        fn arbitrary(g: &mut quickcheck::Gen) -> Self {
            Self {
                connection_id: ConnectionId(I64::new(i64::arbitrary(g))),
                action_placeholder: AnnounceActionPlaceholder::default(),
                transaction_id: TransactionId(I32::new(i32::arbitrary(g))),
                info_hash: InfoHash::arbitrary(g),
                peer_id: PeerId::arbitrary(g),
                bytes_downloaded: NumberOfBytes(I64::new(i64::arbitrary(g))),
                bytes_uploaded: NumberOfBytes(I64::new(i64::arbitrary(g))),
                bytes_left: NumberOfBytes(I64::new(i64::arbitrary(g))),
                event: AnnounceEvent::arbitrary(g).into(),
                ip_address: Ipv4AddrBytes::arbitrary(g),
                key: PeerKey(i32::arbitrary(g).into()),
                peers_wanted: NumberOfPeers(I32::new(i32::arbitrary(g))),
                port: Port(U16::new(u16::arbitrary(g))),
            }
        }
    }

    impl quickcheck::Arbitrary for ScrapeRequest {
        fn arbitrary(g: &mut quickcheck::Gen) -> Self {
            let info_hashes = (0..u8::arbitrary(g))
                .map(|_| InfoHash::arbitrary(g))
                .collect();

            Self {
                connection_id: ConnectionId(I64::new(i64::arbitrary(g))),
                transaction_id: TransactionId(I32::new(i32::arbitrary(g))),
                info_hashes,
            }
        }
    }

    fn same_after_conversion(request: Request) -> bool {
        let mut buf = Vec::new();

        request.clone().write_bytes(&mut buf).unwrap();
        let r2 = Request::parse_bytes(&buf[..], ::std::u8::MAX).unwrap();

        let success = request == r2;

        if !success {
            ::pretty_assertions::assert_eq!(request, r2);
        }

        success
    }

    #[quickcheck]
    fn test_connect_request_convert_identity(request: ConnectRequest) -> bool {
        same_after_conversion(request.into())
    }

    #[quickcheck]
    fn test_announce_request_convert_identity(request: AnnounceRequest) -> bool {
        same_after_conversion(request.into())
    }

    #[quickcheck]
    fn test_scrape_request_convert_identity(request: ScrapeRequest) -> TestResult {
        if request.info_hashes.is_empty() {
            return TestResult::discard();
        }

        TestResult::from_bool(same_after_conversion(request.into()))
    }
}
