//! Loopback HTTP and desktop launch facilities owned by the platform seam.
use std::io;

pub(crate) async fn bind_loopback(port: u16) -> io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await
}

#[cfg(test)]
pub(crate) async fn connect_loopback(
    address: std::net::SocketAddr,
) -> io::Result<tokio::net::TcpStream> {
    tokio::net::TcpStream::connect(address).await
}

pub(crate) fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}
