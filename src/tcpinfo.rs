//! Kernel TCP statistics of a connected socket: round-trip time and
//! retransmissions, used to tell a lossy path from a distant or slow server.

use std::net::TcpStream;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TcpStats {
    /// Smoothed round-trip time, in microseconds.
    pub rtt_us: u32,
    /// Data sent since the connection opened, and how much of it was retransmitted.
    /// Bytes on Windows and macOS, segments on Linux; only their ratio is meaningful.
    pub sent: u64,
    pub retransmitted: u64,
}

/// Reads the statistics of `sock`. None if the OS does not provide them.
#[cfg(windows)]
pub fn read(sock: &TcpStream) -> Option<TcpStats> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{WSAIoctl, SIO_TCP_INFO, SOCKET, TCP_INFO_v0};
    // SIO_TCP_INFO (Windows 10 1703+): the input is the structure version.
    let version: u32 = 0;
    let mut info: TCP_INFO_v0 = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    let rc = unsafe {
        WSAIoctl(
            sock.as_raw_socket() as SOCKET,
            SIO_TCP_INFO,
            &version as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
            &mut info as *mut TCP_INFO_v0 as *mut _,
            std::mem::size_of::<TCP_INFO_v0>() as u32,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    };
    (rc == 0).then(|| TcpStats { rtt_us: info.RttUs, sent: info.BytesOut, retransmitted: info.BytesRetrans as u64 })
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn read(sock: &TcpStream) -> Option<TcpStats> {
    use std::os::fd::AsRawFd;
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            &mut info as *mut libc::tcp_info as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then(|| TcpStats {
        rtt_us: info.tcpi_rtt,
        sent: info.tcpi_segs_out as u64,
        retransmitted: info.tcpi_total_retrans as u64,
    })
}

#[cfg(target_os = "macos")]
pub fn read(sock: &TcpStream) -> Option<TcpStats> {
    use std::os::fd::AsRawFd;
    let mut info: libc::tcp_connection_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_connection_info>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_CONNECTION_INFO,
            &mut info as *mut libc::tcp_connection_info as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then(|| TcpStats {
        rtt_us: info.tcpi_srtt.saturating_mul(1000),
        sent: info.tcpi_txbytes,
        retransmitted: info.tcpi_txretransmitbytes,
    })
}

#[cfg(not(any(windows, all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
pub fn read(_sock: &TcpStream) -> Option<TcpStats> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn reads_stats_of_a_loopback_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let mut total = 0;
            while total < 1_000_000 {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(&vec![7u8; 1_000_000]).unwrap();
        server.join().unwrap();
        let stats = read(&c);
        if cfg!(any(windows, all(target_os = "linux", target_env = "gnu"), target_os = "macos")) {
            let stats = stats.expect("TCP statistics");
            assert!(stats.sent > 0, "{stats:?}");
            assert!(stats.retransmitted <= stats.sent, "{stats:?}");
        }
    }
}
