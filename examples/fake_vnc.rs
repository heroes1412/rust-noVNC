//! A minimal RFB (VNC) server used to try the proxy without installing a real
//! VNC server.
//!
//! It speaks just enough of the protocol for noVNC to connect: version
//! handshake, "None" security, ServerInit, and a raw full-frame update on every
//! FramebufferUpdateRequest.
//!
//! ```text
//! cargo run --example fake_vnc -- 127.0.0.1:5900
//! cargo run -- --vnc 127.0.0.1:5900
//! ```
//!
//! Then open <http://127.0.0.1:6080/vnc.html?autoconnect=true>.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WIDTH: u16 = 640;
const HEIGHT: u16 = 480;

#[tokio::main]
async fn main() -> io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:5900".to_string());
    let listener = TcpListener::bind(&addr).await?;
    println!("fake VNC server on {}", listener.local_addr()?);

    loop {
        let (stream, peer) = listener.accept().await?;
        println!("client connected: {peer}");
        tokio::spawn(async move {
            if let Err(err) = serve(stream).await {
                println!("{peer}: {err}");
            }
            println!("client gone: {peer}");
        });
    }
}

async fn serve(mut socket: TcpStream) -> io::Result<()> {
    socket.write_all(b"RFB 003.008\n").await?;

    let mut version = [0u8; 12];
    socket.read_exact(&mut version).await?;

    // One security type: None (1).
    socket.write_all(&[1, 1]).await?;
    let mut chosen = [0u8; 1];
    socket.read_exact(&mut chosen).await?;

    socket.write_all(&0u32.to_be_bytes()).await?; // SecurityResult: OK

    let mut shared = [0u8; 1];
    socket.read_exact(&mut shared).await?;

    // ServerInit, 32 bpp little-endian true colour with R/G/B in the low
    // three bytes -- exactly what noVNC asks for in SetPixelFormat.
    let mut init = Vec::new();
    init.extend_from_slice(&WIDTH.to_be_bytes());
    init.extend_from_slice(&HEIGHT.to_be_bytes());
    init.extend_from_slice(&[
        32, 24, 0, 1, // bpp, depth, big-endian, true colour
        0, 255, 0, 255, 0, 255, // max R, G, B
        0, 8, 16, // shifts R, G, B
        0, 0, 0, // padding
    ]);
    let name = b"novnc-rs fake VNC server";
    init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    init.extend_from_slice(name);
    socket.write_all(&init).await?;

    let frame = paint();
    let mut message = [0u8; 1];

    loop {
        socket.read_exact(&mut message).await?;
        match message[0] {
            // SetPixelFormat
            0 => {
                let mut rest = [0u8; 19];
                socket.read_exact(&mut rest).await?;
            }
            // SetEncodings (we always answer with raw)
            2 => {
                let mut header = [0u8; 3];
                socket.read_exact(&mut header).await?;
                let count = u16::from_be_bytes([header[1], header[2]]) as usize;
                let mut encodings = vec![0u8; count * 4];
                socket.read_exact(&mut encodings).await?;
            }
            // FramebufferUpdateRequest
            3 => {
                let mut rest = [0u8; 9];
                socket.read_exact(&mut rest).await?;
                send_frame(&mut socket, &frame).await?;
            }
            // KeyEvent
            4 => {
                let mut rest = [0u8; 7];
                socket.read_exact(&mut rest).await?;
            }
            // PointerEvent
            5 => {
                let mut rest = [0u8; 5];
                socket.read_exact(&mut rest).await?;
            }
            // ClientCutText
            6 => {
                let mut header = [0u8; 7];
                socket.read_exact(&mut header).await?;
                let length =
                    u32::from_be_bytes([header[3], header[4], header[5], header[6]]) as usize;
                let mut text = vec![0u8; length];
                socket.read_exact(&mut text).await?;
            }
            other => {
                println!("ignoring unknown client message {other}");
                return Ok(());
            }
        }
    }
}

async fn send_frame(socket: &mut TcpStream, frame: &[u8]) -> io::Result<()> {
    let mut header = Vec::with_capacity(16);
    header.push(0); // FramebufferUpdate
    header.push(0); // padding
    header.extend_from_slice(&1u16.to_be_bytes()); // one rectangle
    header.extend_from_slice(&0u16.to_be_bytes()); // x
    header.extend_from_slice(&0u16.to_be_bytes()); // y
    header.extend_from_slice(&WIDTH.to_be_bytes());
    header.extend_from_slice(&HEIGHT.to_be_bytes());
    header.extend_from_slice(&0i32.to_be_bytes()); // Raw encoding
    socket.write_all(&header).await?;
    socket.write_all(frame).await
}

/// RGBA test pattern: colour gradient, checkerboard and a border, so a
/// mis-ordered pixel format is immediately visible.
fn paint() -> Vec<u8> {
    let mut frame = Vec::with_capacity(WIDTH as usize * HEIGHT as usize * 4);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let checker = ((x / 32) + (y / 32)) % 2 == 0;
            let border = x < 4 || y < 4 || x >= WIDTH - 4 || y >= HEIGHT - 4;
            let (r, g, b) = if border {
                (255, 255, 255)
            } else if checker {
                (
                    (x.saturating_mul(255) / WIDTH) as u8,
                    (y.saturating_mul(255) / HEIGHT) as u8,
                    180,
                )
            } else {
                (40, 60, (x ^ y) as u8)
            };
            frame.extend_from_slice(&[r, g, b, 0]);
        }
    }
    frame
}
