use std::io::ErrorKind;

use conde::server::read_frame;

#[tokio::test]
async fn newline_terminated_frame_over_max_is_refused() {
    let mut input = &b"abcdef\n"[..];
    let err = read_frame(&mut input, 5).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData);
}

#[tokio::test]
async fn unterminated_frame_over_max_is_refused() {
    let mut input = &b"abcdef"[..];
    assert_eq!(
        read_frame(&mut input, 5).await.unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[tokio::test]
async fn frames_up_to_max_are_split_on_newlines() {
    let mut input = &b"abcde\nxy\ntail"[..];
    assert_eq!(read_frame(&mut input, 5).await.unwrap().unwrap(), b"abcde");
    assert_eq!(read_frame(&mut input, 5).await.unwrap().unwrap(), b"xy");
    assert_eq!(read_frame(&mut input, 5).await.unwrap().unwrap(), b"tail");
    assert_eq!(read_frame(&mut input, 5).await.unwrap(), None);
}
