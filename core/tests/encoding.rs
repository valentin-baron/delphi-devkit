use ddk_core::encoding::{decode_bytes, encode_string};

#[test]
fn decode_utf8_ascii() {
    assert_eq!(decode_bytes(b"Hello, world!", "utf-8"), "Hello, world!");
}

#[test]
fn decode_utf8_label_variant() {
    assert_eq!(decode_bytes(b"abc", "utf8"), "abc");
}

#[test]
fn decode_utf8_with_multibyte() {
    let input = "café".as_bytes();
    assert_eq!(decode_bytes(input, "utf-8"), "café");
}

#[test]
fn decode_utf8_lossy_on_invalid() {
    let input = &[0xFF, 0xFE, 0x41];
    let result = decode_bytes(input, "utf-8");
    assert!(result.contains('A'));
}

#[test]
fn decode_windows_1252() {
    // In Windows-1252, 0xE9 is 'é'
    let input = &[0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0xE9]; // "Hello é"
    let result = decode_bytes(input, "windows-1252");
    assert_eq!(result, "Hello é");
}

#[test]
fn decode_case_insensitive_label() {
    let input = &[0x41]; // 'A'
    assert_eq!(decode_bytes(input, "Windows-1252"), "A");
    assert_eq!(decode_bytes(input, "WINDOWS-1252"), "A");
}

#[test]
fn decode_utf32le_ascii() {
    // 'A' in UTF-32LE
    let input = &[0x41, 0x00, 0x00, 0x00];
    assert_eq!(decode_bytes(input, "utf-32le"), "A");
}

#[test]
fn decode_utf32be_ascii() {
    // 'A' in UTF-32BE
    let input = &[0x00, 0x00, 0x00, 0x41];
    assert_eq!(decode_bytes(input, "utf-32be"), "A");
}

#[test]
fn decode_utf32le_multibyte() {
    // 'é' (U+00E9) in UTF-32LE
    let input = &[0xE9, 0x00, 0x00, 0x00];
    assert_eq!(decode_bytes(input, "utf-32le"), "é");
}

#[test]
fn decode_utf32_invalid_codepoint_uses_replacement() {
    // 0xFFFFFFFF is not a valid Unicode codepoint
    let input = &[0xFF, 0xFF, 0xFF, 0xFF];
    let result = decode_bytes(input, "utf-32le");
    assert_eq!(result, "\u{FFFD}");
}

#[test]
fn decode_utf32_remainder_bytes_ignored() {
    // 5 bytes: first 4 decode as 'A', the 5th trailing byte is ignored by chunks_exact
    let input = &[0x41, 0x00, 0x00, 0x00, 0x99];
    let result = decode_bytes(input, "utf-32le");
    assert_eq!(result, "A");
}

#[test]
fn decode_unknown_label_falls_back_to_utf8_lossy() {
    let input = b"plain ascii";
    assert_eq!(decode_bytes(input, "totally-fake-encoding"), "plain ascii");
}

#[test]
fn encode_utf8() {
    let result = encode_string("Hello", "utf-8");
    assert_eq!(result, b"Hello");
}

#[test]
fn encode_utf8_label_variant() {
    let result = encode_string("abc", "utf8");
    assert_eq!(result, b"abc");
}

#[test]
fn encode_utf32le() {
    let result = encode_string("A", "utf-32le");
    assert_eq!(result, vec![0x41, 0x00, 0x00, 0x00]);
}

#[test]
fn encode_utf32be() {
    let result = encode_string("A", "utf-32be");
    assert_eq!(result, vec![0x00, 0x00, 0x00, 0x41]);
}

#[test]
fn encode_utf32le_emoji() {
    // '😀' is U+1F600
    let result = encode_string("😀", "utf-32le");
    assert_eq!(result, vec![0x00, 0xF6, 0x01, 0x00]);
}

#[test]
fn encode_windows_1252() {
    let result = encode_string("é", "windows-1252");
    assert_eq!(result, vec![0xE9]);
}

#[test]
fn roundtrip_utf8() {
    let original = "Hello café!";
    let encoded = encode_string(original, "utf-8");
    let decoded = decode_bytes(&encoded, "utf-8");
    assert_eq!(decoded, original);
}

#[test]
fn roundtrip_utf32le() {
    let original = "Test 😀";
    let encoded = encode_string(original, "utf-32le");
    let decoded = decode_bytes(&encoded, "utf-32le");
    assert_eq!(decoded, original);
}

#[test]
fn roundtrip_utf32be() {
    let original = "Test 😀";
    let encoded = encode_string(original, "utf-32be");
    let decoded = decode_bytes(&encoded, "utf-32be");
    assert_eq!(decoded, original);
}

#[test]
fn roundtrip_windows_1252() {
    let original = "Héllo Wörld";
    let encoded = encode_string(original, "windows-1252");
    let decoded = decode_bytes(&encoded, "windows-1252");
    assert_eq!(decoded, original);
}

#[test]
fn encode_unknown_label_returns_utf8() {
    let result = encode_string("abc", "nonsense-encoding");
    assert_eq!(result, b"abc");
}

#[test]
fn decode_oem_ascii() {
    // ASCII is identical in every OEM codepage, so this holds on any machine.
    let result = decode_bytes(b"Hello", "oem");
    assert_eq!(result, "Hello");
}

#[test]
fn encode_oem_ascii() {
    let result = encode_string("Hello", "oem");
    assert_eq!(result, b"Hello");
}
