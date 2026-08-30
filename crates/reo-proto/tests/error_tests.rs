use reo_proto::BcError;

#[test]
fn test_error_display() {
    let e = BcError::BadMagic([0xDE, 0xAD, 0xBE, 0xEF]);
    let s = format!("{e}");
    assert!(s.contains("de ad be ef"));

    let e2 = BcError::BufferTooSmall {
        needed: 100,
        available: 50,
    };
    let s2 = format!("{e2}");
    assert!(s2.contains("100"));
    assert!(s2.contains("50"));
}

#[test]
fn test_error_display_all_variants() {
    let errors: Vec<BcError> = vec![
        BcError::Incomplete,
        BcError::BadMagic([0x01, 0x02, 0x03, 0x04]),
        BcError::InvalidHeader("test field"),
        BcError::Encryption("bad key"),
        BcError::XmlParse("missing tag"),
        BcError::Protocol("unexpected state"),
        BcError::AuthFailed(401),
        BcError::WrongRole,
        BcError::BufferTooSmall {
            needed: 256,
            available: 128,
        },
        BcError::MessageTooLarge {
            size: 1_000_000,
            max: 512_000,
        },
        BcError::MediaFrameTooLarge {
            size: 5_000_000,
            max: 4_194_304,
        },
    ];

    for e in &errors {
        let s = format!("{e}");
        assert!(!s.is_empty(), "Display for {e:?} should not be empty");
    }

    // Verify specific content
    assert!(format!("{}", errors[0]).contains("incomplete"));
    assert!(format!("{}", errors[3]).contains("bad key"));
    assert!(format!("{}", errors[4]).contains("missing tag"));
    assert!(format!("{}", errors[5]).contains("unexpected state"));
    assert!(format!("{}", errors[6]).contains("401"));
    assert!(format!("{}", errors[7]).contains("role"));
}

#[test]
fn test_error_is_std_error() {
    let e: Box<dyn std::error::Error> = Box::new(BcError::Incomplete);
    assert!(format!("{e}").contains("incomplete"));
}
