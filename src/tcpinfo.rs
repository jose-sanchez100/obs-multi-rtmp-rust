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

/// `struct tcp_connection_info` from XNU's bsd/netinet/tcp.h. Not libc's: it
/// declares the fifteen 1-bit TFO bitfields as separate u32 fields, so its struct
/// is 120 bytes instead of 112 and the counters after them read past what the
/// kernel fills (always 0).
#[cfg(target_os = "macos")]
#[repr(C)]
#[allow(dead_code)] // only some fields are read; all are needed for the layout
struct TcpConnectionInfo {
    state: u8,
    snd_wscale: u8,
    rcv_wscale: u8,
    pad1: u8,
    options: u32,
    flags: u32,
    rto: u32,
    maxseg: u32,
    snd_ssthresh: u32,
    snd_cwnd: u32,
    snd_wnd: u32,
    snd_sbbytes: u32,
    rcv_wnd: u32,
    rttcur: u32,
    /// Smoothed RTT in ms.
    srtt: u32,
    rttvar: u32,
    /// The TFO bitfields.
    tfo: u32,
    txpackets: u64,
    txbytes: u64,
    txretransmitbytes: u64,
    rxpackets: u64,
    rxbytes: u64,
    rxoutoforderbytes: u64,
    txretransmitpackets: u64,
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<TcpConnectionInfo>() == 112);

#[cfg(target_os = "macos")]
pub fn read(sock: &TcpStream) -> Option<TcpStats> {
    use std::os::fd::AsRawFd;
    let mut info: TcpConnectionInfo = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<TcpConnectionInfo>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_CONNECTION_INFO,
            &mut info as *mut TcpConnectionInfo as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then(|| TcpStats {
        rtt_us: info.srtt.saturating_mul(1000),
        sent: info.txbytes,
        retransmitted: info.txretransmitbytes,
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
