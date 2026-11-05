//! Inputs the fuzzers found, kept so that they stay fixed.

use ruthos_dicom::deid::DateShift;

/// A bare data set whose stray item delimiter hid an element claiming 2.8 GB.
#[test]
fn a_stray_item_delimiter_cannot_hide_a_huge_element() {
    let bytes = [
        0x18, 0, 0x11, 0x60, 0, 0xff, 0xdd, 0xe0, 0, 0, 0, 0, 0xfe, 0xff, 0x0d, 0xe0, 0x53, 0x51, 0x73, 0, 0xfe, 0xff,
        0xdd, 0xe0, 0, 0, 0x47, 0xab, 0,
    ];
    assert!(!ruthos_dicom::structure::bare_holds(&bytes, true));
    assert!(ruthos_dicom::open::open_from_bytes(&bytes).is_err());
}

/// Byte 8 of a date and time falling inside a multi-byte character.
#[test]
fn a_date_time_that_is_not_ascii_does_not_panic() {
    let shift = DateShift::new(30, false);
    assert_eq!(shift.shift_dt("2026010\u{ab}"), None);
    assert_eq!(shift.shift_dt("\u{ab}\u{ab}\u{ab}\u{ab}\u{ab}"), None);
    assert!(shift.shift_dt("20260110120000").is_some());
}
