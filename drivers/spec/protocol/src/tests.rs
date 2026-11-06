use super::*;

/// The `2+2` exchange in the `server` help page's hex dump (debug 0xC00000).
fn manual_request() -> Vec<u8> {
    let mut b = vec![
        0xce, 0xfa, 0xed, 0xfe, 0x04, 0x00, 0x00, 0x00, 0x84, 0x00, 0x00, 0x00, 0xa3, 0x01, 0x00,
        0x00, 0x88, 0x9b, 0x7e, 0x5c, 0x5a, 0xfd, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    ];
    b.resize(132, 0);
    b.extend_from_slice(b"2+2\0");
    b
}

fn manual_reply() -> Vec<u8> {
    let mut b = vec![
        0xce, 0xfa, 0xed, 0xfe, 0x04, 0x00, 0x00, 0x00, 0x84, 0x00, 0x00, 0x00, 0xa3, 0x01, 0x00,
        0x00, 0x88, 0x9b, 0x7e, 0x5c, 0xca, 0xfe, 0x01, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x02, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    ];
    b.resize(132, 0);
    b.extend_from_slice(b"4\0");
    b
}

const LITTLE_V4: Framing = Framing {
    endian: Endian::Little,
    version: Version::V4,
};

async fn read_all(bytes: &[u8]) -> Vec<Result<(Packet, Framing), ReadError>> {
    let mut r = bytes;
    let mut out = Vec::new();
    while !r.is_empty() {
        let res = read_packet(&mut r).await;
        let fatal = res.as_ref().is_err_and(ReadError::is_fatal);
        out.push(res);
        if fatal {
            break;
        }
    }
    out
}

#[tokio::test]
async fn manual_request_decodes() {
    let (p, framing) = read_packet(&mut &manual_request()[..]).await.unwrap();
    assert_eq!(framing, LITTLE_V4);
    let h = &p.header;
    assert_eq!(
        (h.sn, h.sec, h.usec, h.cmd, h.data_type),
        (
            0x1a3,
            0x5c7e_9b88,
            0x01_fd5a,
            Cmd::CmdWithReturn,
            Some(DataType::String)
        )
    );
    assert_eq!(
        (h.rows, h.cols, h.err, h.flags, h.name.as_str()),
        (0, 0, 0, 0, "")
    );
    assert_eq!(p.text(), "2+2");
}

#[tokio::test]
async fn sv_deleted_is_flags_bit_0() {
    let mut b = manual_request();
    b[48] = 1; // flags, the 13th word of a v4 header
    let (p, _) = read_packet(&mut &b[..]).await.unwrap();
    assert_ne!(p.header.flags & FLAG_DELETED, 0);
}

#[tokio::test]
async fn manual_reply_encodes_byte_for_byte() {
    let (req, framing) = read_packet(&mut &manual_request()[..]).await.unwrap();
    let mut reply = Packet::reply_to(&req.header).with_number(4.0);
    reply.header.usec = 0x01_feca;
    reply.header.sec = 0x5c7e_9b88;
    assert_eq!(encode(&reply, framing).unwrap(), manual_reply());
}

#[tokio::test]
async fn every_framing_round_trips() {
    let mut p = Packet::new(Cmd::Event, 7, "motor/tth/position").with_number(-12.5);
    p.header.err = 3;
    p.header.flags = FLAG_DELETED;
    for endian in [Endian::Little, Endian::Big] {
        for (version, size) in [(Version::V2, 124), (Version::V3, 128), (Version::V4, 132)] {
            let framing = Framing { endian, version };
            let bytes = encode(&p, framing).unwrap();
            assert_eq!(bytes.len(), size + p.data.len());
            let (q, got) = read_packet(&mut &bytes[..]).await.unwrap();
            assert_eq!(got, framing);
            let mut want = p.clone();
            if version < Version::V3 {
                want.header.err = 0;
            }
            if version < Version::V4 {
                want.header.flags = 0;
            }
            assert_eq!(q, want, "{framing:?}");
        }
    }
}

#[tokio::test]
async fn binary_data_is_native_on_both_sides_of_a_foreign_peer() {
    let mut p = Packet::new(Cmd::ChanSend, 1, "var/DEGC");
    p.header.data_type = Some(DataType::Double);
    p.data = 297.25f64.to_ne_bytes().to_vec();
    for endian in [Endian::Little, Endian::Big] {
        let bytes = encode(
            &p,
            Framing {
                endian,
                version: Version::V4,
            },
        )
        .unwrap();
        let wire: [u8; 8] = bytes[132..].try_into().unwrap();
        let on_wire = match endian {
            Endian::Little => f64::from_le_bytes(wire),
            Endian::Big => f64::from_be_bytes(wire),
        };
        assert_eq!(on_wire, 297.25);
        let (q, _) = read_packet(&mut &bytes[..]).await.unwrap();
        assert_eq!(q.number(), Some(297.25));
    }
}

#[tokio::test]
async fn later_version_is_read_as_v4_with_extra_header_skipped() {
    let mut bytes = encode(&Packet::new(Cmd::Hello, 9, "hi"), LITTLE_V4).unwrap();
    bytes[4] = 5;
    bytes[8] = 140;
    bytes.splice(132..132, [0xaa; 8]);
    bytes.extend(encode(&Packet::new(Cmd::Close, 10, ""), LITTLE_V4).unwrap());
    let got = read_all(&bytes).await;
    let sns: Vec<_> = got
        .iter()
        .map(|r| r.as_ref().unwrap().0.header.sn)
        .collect();
    assert_eq!(sns, [9, 10]);
    assert_eq!(got[0].as_ref().unwrap().0.header.name, "hi");
}

#[tokio::test]
async fn unknown_code_consumes_its_packet() {
    let mut bytes = encode(
        &Packet::new(Cmd::Event, 1, "x").with_string("abc"),
        LITTLE_V4,
    )
    .unwrap();
    bytes[24] = 99;
    bytes.extend(encode(&Packet::new(Cmd::Close, 2, ""), LITTLE_V4).unwrap());
    let got = read_all(&bytes).await;
    assert!(matches!(
        got[0],
        Err(ReadError::Unknown {
            sn: 1,
            cmd: 99,
            data_type: 2,
            ..
        })
    ));
    assert_eq!(got[1].as_ref().unwrap().0.header.cmd, Cmd::Close);
}

#[tokio::test]
async fn bad_preamble_is_fatal() {
    let mut bytes = manual_request();
    bytes[0] = 0;
    let err = read_packet(&mut &bytes[..]).await.unwrap_err();
    assert!(matches!(err, ReadError::BadMagic(_)) && err.is_fatal());

    let mut bytes = manual_request();
    bytes[8] = 100;
    let err = read_packet(&mut &bytes[..]).await.unwrap_err();
    assert!(matches!(err, ReadError::BadSize { vers: 4, size: 100 }));

    let mut bytes = manual_request();
    bytes[40..44].copy_from_slice(&(MAX_DATA + 1).to_le_bytes());
    assert!(matches!(
        read_packet(&mut &bytes[..]).await.unwrap_err(),
        ReadError::TooLong(_)
    ));

    let bytes = manual_request();
    let err = read_packet(&mut &bytes[..134]).await.unwrap_err();
    assert!(matches!(err, ReadError::Io(ref e) if e.kind() == io::ErrorKind::UnexpectedEof));
}

#[test]
fn name_must_fit_the_field() {
    let ok = Packet::new(Cmd::ChanRead, 1, "a".repeat(NAME_LEN - 1));
    assert!(encode(&ok, LITTLE_V4).is_ok());
    for name in ["a".repeat(NAME_LEN), "a\0b".to_string()] {
        let e = encode(&Packet::new(Cmd::ChanRead, 1, name), LITTLE_V4).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }
}

#[test]
fn numbers_are_percent_15g() {
    for (x, s) in [
        (4.0, "4"),
        (0.1, "0.1"),
        (-0.0, "-0"),
        (1.0 / 3.0, "0.333333333333333"),
        (1e5, "100000"),
        (1e15, "1e+15"),
        (1e-5, "1e-05"),
        (123456.789, "123456.789"),
    ] {
        assert_eq!(format_number(x), s);
    }
    assert_eq!(parse_number(" 1e+05\n"), Some(1e5));
    assert_eq!(parse_number("x"), None);
}

#[test]
fn text_needs_no_nul() {
    let mut p = Packet::new(Cmd::ChanSend, 1234, "motor/tth/start_one");
    p.header.data_type = Some(DataType::String);
    p.data = b"12.5".to_vec();
    assert_eq!(p.text(), "12.5");
    assert_eq!(p.number(), Some(12.5));
}

#[test]
fn assoc_round_trips() {
    let p = Packet::new(Cmd::Reply, 1, "var/A").with_assoc([("tth", "1.5"), ("th", "")]);
    assert_eq!(p.data, b"tth\x001.5\0th\0\0\0");
    assert_eq!(
        p.assoc(),
        [
            ("tth".to_string(), "1.5".to_string()),
            ("th".to_string(), String::new())
        ]
    );
}
