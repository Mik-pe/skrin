use super::*;
use crate::test_support::{Item, TestStorage};

fn sample() -> (Vec<u8>, usize) {
    let mut image = file_header(Item::SCHEMA);
    image.extend(
        encode_transaction(1, &BTreeMap::from([(1, Some(Item(10))), (2, Some(Item(20)))]))
            .unwrap(),
    );
    let boundary = image.len();
    image.extend(
        encode_transaction(
            2,
            &BTreeMap::from([(1, Some(Item(15))), (2, None), (3, Some(Item(15)))]),
        )
        .unwrap(),
    );
    (image, boundary)
}

fn recover(io: &TestStorage) -> Result<(Wal, Recovered<Item>)> {
    Wal::recover::<Item>(Box::new(io.clone()))
}

#[test]
fn crc_matches_standard_vectors() {
    assert_eq!(checksum(b""), 0);
    assert_eq!(checksum(b"123456789"), 0xcbf4_3926);
    assert_eq!(
        checksum(b"The quick brown fox jumps over the lazy dog"),
        0x414f_a339
    );
}

#[test]
fn version_one_bytes_match_an_independently_encoded_golden_fixture() {
    // Fixture generated with Python struct (little endian) and zlib.crc32,
    // rather than serializing and deserializing with the same Rust code.
    let mut image = file_header(Item::SCHEMA);
    image.extend(encode_transaction(1, &BTreeMap::from([(42, Some(Item(9)))])).unwrap());
    let hex: String = image.iter().map(|byte| format!("{byte:02x}")).collect();
    let fixture = include_str!("../tests/fixtures/format-v1.hex").trim();
    assert_eq!(hex, fixture);
}

#[test]
fn recovery_at_every_byte_boundary_never_applies_half_a_transaction() {
    let (image, boundary) = sample();
    for cutoff in 0..=image.len() {
        let io = TestStorage::new(image[..cutoff].to_vec());
        if cutoff < HEADER_LEN {
            assert!(matches!(recover(&io), Err(Error::Corrupt { .. })));
            assert_eq!(io.image(), image[..cutoff]);
            continue;
        }
        let (wal, recovered) = recover(&io).unwrap();
        let valid = if cutoff == image.len() {
            image.len()
        } else if cutoff >= boundary {
            boundary
        } else {
            HEADER_LEN
        };
        let expected = if valid == image.len() {
            vec![(1, 15), (3, 15)]
        } else if valid == boundary {
            vec![(1, 10), (2, 20)]
        } else {
            Vec::new()
        };
        let actual: Vec<_> = recovered.rows.iter().map(|(&key, row)| (key, row.0)).collect();
        assert_eq!(actual, expected, "cutoff {cutoff}");
        assert_eq!(recovered.discarded, (cutoff - valid) as u64);
        assert_eq!(wal.bytes, valid as u64);
        assert_eq!(io.image(), image[..valid]);
        assert_eq!(io.syncs(), 1);
    }
}

#[test]
fn every_single_bit_flip_in_complete_storage_is_detected_without_repair() {
    let (image, _) = sample();
    for offset in 0..image.len() {
        for bit in 0..8 {
            let mut corrupted = image.clone();
            corrupted[offset] ^= 1 << bit;
            let io = TestStorage::new(corrupted.clone());
            assert!(recover(&io).is_err(), "byte {offset}, bit {bit}");
            assert_eq!(io.image(), corrupted, "byte {offset}, bit {bit}");
            assert_eq!(io.syncs(), 0);
        }
    }
}

fn header_crc(image: &mut [u8]) {
    let crc = checksum(&image[..28]);
    image[28..32].copy_from_slice(&crc.to_le_bytes());
}

fn frame_crc(image: &mut [u8], offset: usize) {
    let crc = checksum(&image[offset..offset + 20]);
    image[offset + 20..offset + 24].copy_from_slice(&crc.to_le_bytes());
}

fn payload_crc(image: &mut [u8], offset: usize) {
    let len = read_u32(&image[offset + 4..]) as usize;
    let start = offset + FRAME_HEADER_LEN;
    let crc = checksum(&image[start..start + len]);
    image[offset + 16..offset + 20].copy_from_slice(&crc.to_le_bytes());
    frame_crc(image, offset);
}

#[test]
fn schema_mismatch_is_detected_before_repairing_a_torn_tail() {
    let (mut image, _) = sample();
    image.pop();
    image[20..24].copy_from_slice(&2u32.to_le_bytes());
    header_crc(&mut image);
    let io = TestStorage::new(image.clone());
    assert!(matches!(recover(&io), Err(Error::SchemaMismatch { .. })));
    assert_eq!(io.image(), image);
}

#[test]
fn table_identity_and_future_storage_versions_are_rejected() {
    let (image, _) = sample();
    let mut wrong_table = image.clone();
    wrong_table[12..20].copy_from_slice(&999u64.to_le_bytes());
    header_crc(&mut wrong_table);
    let io = TestStorage::new(wrong_table.clone());
    assert!(matches!(recover(&io), Err(Error::SchemaMismatch { .. })));
    assert_eq!(io.image(), wrong_table);
    let mut future = image;
    future[8..12].copy_from_slice(&2u32.to_le_bytes());
    header_crc(&mut future);
    let io = TestStorage::new(future.clone());
    assert!(matches!(recover(&io), Err(Error::UnsupportedFormat(2))));
    assert_eq!(io.image(), future);
}

#[test]
fn checksummed_but_invalid_lengths_and_sequences_are_not_torn_tails() {
    let (image, boundary) = sample();
    for len in [0, 3, MAX_TRANSACTION_BYTES as u32 + 1, u32::MAX] {
        let mut invalid = image.clone();
        invalid[boundary + 4..boundary + 8].copy_from_slice(&len.to_le_bytes());
        frame_crc(&mut invalid, boundary);
        let io = TestStorage::new(invalid.clone());
        assert!(matches!(recover(&io), Err(Error::Corrupt { .. })));
        assert_eq!(io.image(), invalid);
    }
    for sequence in [0u64, 1, 3, u64::MAX] {
        let mut invalid = image.clone();
        invalid[boundary + 8..boundary + 16].copy_from_slice(&sequence.to_le_bytes());
        frame_crc(&mut invalid, boundary);
        let io = TestStorage::new(invalid.clone());
        assert!(matches!(recover(&io), Err(Error::Corrupt { .. })));
        assert_eq!(io.image(), invalid);
    }
}

#[test]
fn valid_checksums_do_not_bypass_operation_validation() {
    let (image, boundary) = sample();
    let start = HEADER_LEN + FRAME_HEADER_LEN;
    for case in 0..6 {
        let mut invalid = image[..boundary].to_vec();
        match case {
            0 => invalid[start..start + 4].copy_from_slice(&0u32.to_le_bytes()),
            1 => invalid[start..start + 4].copy_from_slice(&u32::MAX.to_le_bytes()),
            2 => invalid[start + 4] = 99, // Unknown operation tag.
            3 => invalid[start + 26..start + 34].copy_from_slice(&1u64.to_le_bytes()),
            4 => invalid[start + 13..start + 17].copy_from_slice(&u32::MAX.to_le_bytes()),
            _ => invalid[start + 13..start + 17].copy_from_slice(&9u32.to_le_bytes()),
        }
        payload_crc(&mut invalid, HEADER_LEN);
        let io = TestStorage::new(invalid.clone());
        assert!(matches!(recover(&io), Err(Error::Corrupt { .. })), "case {case}");
        assert_eq!(io.image(), invalid);
    }
}

#[test]
fn arbitrary_garbage_after_a_commit_is_not_silently_discarded() {
    let (mut image, _) = sample();
    image.extend_from_slice(b"garbage");
    let io = TestStorage::new(image.clone());
    assert!(matches!(recover(&io), Err(Error::Corrupt { .. })));
    assert_eq!(io.image(), image);
}

#[test]
fn corruption_before_an_incomplete_suffix_does_not_modify_the_file() {
    let (mut image, _) = sample();
    image[HEADER_LEN + FRAME_HEADER_LEN + 10] ^= 1;
    image.extend_from_slice(b"TXN1");
    let io = TestStorage::new(image.clone());
    assert!(matches!(recover(&io), Err(Error::Corrupt { .. })));
    assert_eq!(io.image(), image);
}

#[test]
fn recovery_read_and_truncate_failures_are_reported_not_hidden() {
    let (mut image, _) = sample();
    let io = TestStorage::new(image.clone());
    io.fail_read();
    assert!(matches!(recover(&io), Err(Error::Io(_))));
    assert_eq!(io.image(), image);
    image.pop();
    let io = TestStorage::new(image.clone());
    io.fail_truncate();
    assert!(matches!(recover(&io), Err(Error::Io(_))));
    assert_eq!(io.image(), image);
    io.clear_faults();
    assert!(recover(&io).is_ok());
}
