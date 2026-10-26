//! Authenticate a Unix connection before exchanging protocol or origin data.
use std::io;
use tokio::net::UnixStream;

pub(super) fn verify(stream: &UnixStream) -> io::Result<()> {
    // SAFETY: geteuid takes no arguments and has no failure or ownership case.
    verify_uid(stream, unsafe { libc::geteuid() })
}

fn verify_uid(stream: &UnixStream, expected: libc::uid_t) -> io::Result<()> {
    let credentials = stream.peer_cred().map_err(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Cannot verify local IPC peer credentials",
        )
    })?;
    if credentials.uid() != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Local IPC peer belongs to a different operating system user",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn both_socket_peers_match_effective_uid_without_protocol_bytes() {
        let (left, right) = UnixStream::pair().unwrap();
        verify(&left).unwrap();
        verify(&right).unwrap();
        let mut byte = [0];
        assert_eq!(
            left.try_read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            right.try_read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn different_expected_uid_is_rejected_without_protocol_bytes() {
        let (left, right) = UnixStream::pair().unwrap();
        let uid = left.peer_cred().unwrap().uid();
        let error = verify_uid(&left, uid.wrapping_add(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let mut byte = [0];
        assert_eq!(
            right.try_read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
