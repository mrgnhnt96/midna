//! `stream.attach`: after the JSON answer, the connection is binary. Daemon -> client:
//! `u32 LE len` + `Frame::encode()`, one per credit. Client -> daemon: 1-byte tags
//! (0x01 want, 0x02 resize, 0x03 input, 0x04 key, 0x05 scroll, 0x06 mouse, 0x07 focus,
//! 0x08 paste; see `midna_proto::frame::ClientMsg`). See ARCHITECTURE.md "Frame stream connection".
use crate::daemon::Daemon;
use crate::term::{EngineMsg, RtHandle};
use midna_proto::frame::ClientMsg;
use midna_proto::{Frame, RpcError, StreamAttachParams};
use serde_json::Value;
use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

pub fn prepare(d: &Arc<Daemon>, params: &Value) -> Result<RtHandle, RpcError> {
    let p: StreamAttachParams = serde_json::from_value(params.clone()).map_err(|e| RpcError::bad_params(e.to_string()))?;
    d.rt(&p.session).ok_or_else(|| RpcError::not_found(format!("no running session {}", p.session)))
}

/// `human`: only the GUI may type, paste, click, scroll or resize through a stream. An agent's
/// stream is view-only (it uses `session.input`, which is logged and refuses to answer
/// permission prompts).
pub fn run(h: RtHandle, mut reader: BufReader<UnixStream>, params: &Value, human: bool) {
    let Ok(p) = serde_json::from_value::<StreamAttachParams>(params.clone()) else { return };
    let Ok(mut ws) = reader.get_ref().try_clone() else { return };
    let id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
    if human && p.cols > 0 && p.rows > 0 {
        h.resize(p.cols, p.rows, p.cell_w, p.cell_h);
    }
    let (ftx, frx) = sync_channel::<Frame>(1);
    if h.tx.send(EngineMsg::Attach(id, ftx)).is_err() {
        return;
    }
    // Frame writer: ends when the engine drops our sender (detach / session closed).
    let writer = std::thread::Builder::new().name("stream-writer".into()).spawn(move || {
        let mut buf = Vec::with_capacity(1 << 16);
        for f in frx {
            buf.clear();
            buf.extend_from_slice(&[0; 4]);
            f.encode(&mut buf);
            let len = (buf.len() - 4) as u32;
            buf[..4].copy_from_slice(&len.to_le_bytes());
            if ws.write_all(&buf).is_err() {
                break;
            }
        }
        let _ = ws.shutdown(std::net::Shutdown::Both);
    });
    // Reader: client messages, all through the engine thread so they reach the PTY in the order
    // sent (raw input written here directly could overtake keys still being encoded there:
    // ↓↓ then ctrl-e arrived as ctrl-e ↓↓). It encodes keys, scrolling, mouse, focus and paste
    // for the app's modes; raw input passes through, snapping the viewport back.
    loop {
        let ok = match ClientMsg::read(&mut reader) {
            Ok(Some(ClientMsg::Want)) => h.tx.send(EngineMsg::Want(id)).is_ok(),
            Ok(Some(_)) if !human => true,
            Ok(Some(ClientMsg::Resize { cols, rows, cell_w, cell_h })) => h.tx.send(EngineMsg::SettleResize(cols, rows, cell_w, cell_h)).is_ok(),
            Ok(Some(m)) => h.client(m),
            // End of stream, or an unknown tag (protocol error): drop the stream.
            Ok(None) | Err(_) => false,
        };
        if !ok {
            break;
        }
    }
    let _ = h.tx.send(EngineMsg::Detach(id));
    let _ = reader.get_ref().shutdown(std::net::Shutdown::Both);
    if let Ok(w) = writer {
        let _ = w.join();
    }
}
