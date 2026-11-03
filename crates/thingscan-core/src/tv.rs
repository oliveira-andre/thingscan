//! Samsung TV remote control over the TV's own WebSocket API (Tizen TVs, 2016 on), the local
//! path Samsung's SmartView SDK and TV remotes use:
//! `wss://<tv>:8002/api/v2/channels/samsung.remote.control?name=<base64>&token=<token>`.
//! The first connection makes the TV ask whether to allow this remote; once allowed it
//! answers `ms.channel.connect` with a token that skips the question next time.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::{TcpStream, UdpSocket};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Remote {
    sink: SplitSink<Socket, Message>,
    open: Arc<AtomicBool>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

pub struct Connected {
    pub remote: Remote,
    /// A new pairing token, when the TV issued one.
    pub token: Option<String>,
}

/// Connects to the TV's remote channel. `wait` covers the time someone needs to press
/// Allow on the TV the first time.
pub async fn connect(ip: IpAddr, client_name: &str, token: Option<&str>, wait: Duration) -> Result<Connected> {
    let name = base64::engine::general_purpose::STANDARD.encode(client_name);
    let token_query = token.map(|t| format!("&token={t}")).unwrap_or_default();
    let secure = format!("wss://{ip}:8002/api/v2/channels/samsung.remote.control?name={name}{token_query}");
    // 2018+ TVs need the secure endpoint; older ones only have plain 8001.
    let socket = match open(&secure).await {
        Ok(socket) => socket,
        Err(secure_error) => {
            let plain = format!("ws://{ip}:8001/api/v2/channels/samsung.remote.control?name={name}");
            open(&plain).await.map_err(|_| secure_error)?
        }
    };
    let (sink, mut stream) = socket.split();

    let token = tokio::time::timeout(wait, async {
        while let Some(message) = stream.next().await {
            let message = message.context("the TV dropped the connection")?;
            let Ok(text) = message.to_text() else { continue };
            let Ok(event) = serde_json::from_str::<Value>(text) else { continue };
            match event["event"].as_str() {
                Some("ms.channel.connect") => return Ok(event["data"]["token"].as_str().map(str::to_string)),
                Some("ms.channel.unauthorized") => {
                    bail!("the TV refused this remote; allow Thingscan in the TV's device connection settings")
                }
                Some("ms.error") => bail!("the TV reported an error: {}", event["data"]["message"]),
                _ => {}
            }
        }
        Err(anyhow!("the TV closed the connection"))
    })
    .await
    .map_err(|_| anyhow!("the TV didn't answer; choose Allow on the TV, then try again"))??;

    let open_flag = Arc::new(AtomicBool::new(true));
    let flag = open_flag.clone();
    let reader = tokio::spawn(async move {
        // Keep reading so pings get answered and a closed socket is noticed.
        while let Some(Ok(message)) = stream.next().await {
            if message.is_close() {
                break;
            }
        }
        flag.store(false, Ordering::Relaxed);
    });
    Ok(Connected { remote: Remote { sink, open: open_flag, reader }, token })
}

async fn open(url: &str) -> Result<Socket> {
    // TVs present a certificate from Samsung's private "SmartViewSDK Root CA", which no
    // system trust store has; the pairing token is what authenticates this connection.
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?;
    let connecting = tokio_tungstenite::connect_async_tls_with_config(url, None, true, Some(Connector::NativeTls(tls)));
    let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, connecting)
        .await
        .map_err(|_| anyhow!("the TV didn't answer; it may be off"))?
        .map_err(|e| anyhow!("couldn't reach the TV's remote service: {e}"))?;
    Ok(socket)
}

impl Remote {
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// A short press of a remote key such as `KEY_VOLUP`.
    pub async fn click(&mut self, key: &str) -> Result<()> {
        self.send(key_command("Click", key)?).await
    }

    /// Puts a key down without letting go. A single press held for a while reads as a short
    /// one; sent again and again until [`Remote::release`], the way a real remote repeats a
    /// held key, it's a long press: a few seconds of `KEY_POWER` turns a Frame TV fully off.
    pub async fn press(&mut self, key: &str) -> Result<()> {
        self.send(key_command("Press", key)?).await
    }

    /// Lets go of a key put down with [`Remote::press`].
    pub async fn release(&mut self, key: &str) -> Result<()> {
        self.send(key_command("Release", key)?).await
    }

    /// Moves the TV's on-screen pointer by a relative amount (the touchpad).
    pub async fn move_pointer(&mut self, dx: i32, dy: i32) -> Result<()> {
        self.send(pointer_move(dx, dy)).await
    }

    /// Clicks where the pointer is.
    pub async fn pointer_click(&mut self) -> Result<()> {
        self.send(json!({
            "method": "ms.remote.control",
            "params": { "Cmd": "LeftClick", "TypeOfRemote": "ProcessMouseDevice" },
        }))
        .await
    }

    /// Opens an installed app by its Tizen app id.
    pub async fn launch(&mut self, app_id: &str) -> Result<()> {
        if app_id.is_empty() || !app_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
            bail!("not an app id: {app_id}");
        }
        self.send(json!({
            "method": "ms.channel.emit",
            "params": {
                "event": "ed.apps.launch",
                "to": "host",
                "data": { "action_type": "DEEP_LINK", "appId": app_id, "metaTag": "" },
            },
        }))
        .await
    }

    async fn send(&mut self, payload: Value) -> Result<()> {
        if !self.is_open() {
            bail!("the TV closed the connection");
        }
        self.sink.send(Message::text(payload.to_string())).await.context("couldn't send to the TV")
    }
}

/// `KEY_` followed by capitals, digits or underscores.
pub fn is_key(key: &str) -> bool {
    key.strip_prefix("KEY_").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 32
            && rest.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    })
}

pub fn key_command(cmd: &str, key: &str) -> Result<Value> {
    if !is_key(key) {
        bail!("not a remote key: {key}");
    }
    Ok(json!({
        "method": "ms.remote.control",
        "params": { "Cmd": cmd, "DataOfCmd": key, "Option": "false", "TypeOfRemote": "SendRemoteKey" },
    }))
}

/// A relative pointer move, as the TV's touch remote sends it.
pub fn pointer_move(dx: i32, dy: i32) -> Value {
    json!({
        "method": "ms.remote.control",
        "params": { "Cmd": "Move", "Position": { "x": dx, "y": dy, "Time": "0" }, "TypeOfRemote": "ProcessMouseDevice" },
    })
}

/// Wake-on-LAN: TVs with "Power on with mobile" enabled turn on for this packet.
pub async fn wake(mac: &str) -> Result<()> {
    let packet = magic_packet(parse_mac(mac)?);
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_broadcast(true)?;
    let mut targets = vec![Ipv4Addr::BROADCAST];
    for iface in if_addrs::get_if_addrs()? {
        if let (false, if_addrs::IfAddr::V4(v4)) = (iface.is_loopback(), &iface.addr)
            && let Some(broadcast) = v4.broadcast
        {
            targets.push(broadcast);
        }
    }
    let mut sent = false;
    for target in targets {
        for port in [9, 7] {
            sent |= socket.send_to(&packet, (target, port)).await.is_ok();
        }
    }
    if !sent {
        bail!("couldn't send the wake-up packet");
    }
    Ok(())
}

/// The MAC address the system ARP cache holds for `ip`, if it talked to it recently.
pub async fn arp_mac(ip: IpAddr) -> Option<String> {
    let out = tokio::process::Command::new("arp").args(["-n", &ip.to_string()]).output().await.ok()?;
    mac_in_arp_output(&String::from_utf8_lossy(&out.stdout))
}

/// Finds the MAC in `arp -n` output (macOS: `? (ip) at 4:e4:b6:b3:90:82 on en0`,
/// Linux: `ip ether 04:e4:b6:b3:90:82 C eth0`), zero-padded and upper-case.
pub fn mac_in_arp_output(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|word| {
        let parts: Vec<&str> = word.split(':').collect();
        let valid =
            parts.len() == 6 && parts.iter().all(|p| (1..=2).contains(&p.len()) && u8::from_str_radix(p, 16).is_ok());
        valid.then(|| parts.iter().map(|p| format!("{:0>2}", p.to_uppercase())).collect::<Vec<_>>().join(":"))
    })
}

pub fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = mac.split([':', '-']).collect();
    if parts.len() != 6 {
        bail!("not a MAC address: {mac}");
    }
    let mut out = [0u8; 6];
    for (byte, part) in out.iter_mut().zip(parts) {
        *byte = u8::from_str_radix(part, 16).with_context(|| format!("not a MAC address: {mac}"))?;
    }
    Ok(out)
}

pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_key_commands_and_rejects_others() {
        let cmd = key_command("Click", "KEY_VOLUP").unwrap();
        assert_eq!(cmd["method"], "ms.remote.control");
        assert_eq!(cmd["params"]["DataOfCmd"], "KEY_VOLUP");
        assert_eq!(cmd["params"]["TypeOfRemote"], "SendRemoteKey");
        assert!(is_key("KEY_0"));
        assert!(!is_key("KEY_"));
        assert!(!is_key("VOLUP"));
        assert!(!is_key("KEY_vol\"up"));
        assert!(key_command("Click", "rm -rf").is_err());
    }

    #[test]
    fn reads_macs_from_arp_output() {
        assert_eq!(
            mac_in_arp_output("? (192.168.3.16) at 4:e4:b6:b3:90:82 on en0 ifscope [ethernet]\n").as_deref(),
            Some("04:E4:B6:B3:90:82")
        );
        assert_eq!(
            mac_in_arp_output("192.168.3.16  ether  04:e4:b6:b3:90:82  C  eth0").as_deref(),
            Some("04:E4:B6:B3:90:82")
        );
        assert_eq!(mac_in_arp_output("? (192.168.3.16) at (incomplete) on en0 ifscope [ethernet]"), None);
    }

    #[test]
    fn builds_pointer_moves() {
        let cmd = pointer_move(12, -7);
        assert_eq!(cmd["params"]["TypeOfRemote"], "ProcessMouseDevice");
        assert_eq!(cmd["params"]["Cmd"], "Move");
        assert_eq!(cmd["params"]["Position"]["x"], 12);
        assert_eq!(cmd["params"]["Position"]["y"], -7);
    }

    #[test]
    fn builds_wake_on_lan_packets() {
        let mac = parse_mac("04:E4:B6:B3:90:82").unwrap();
        assert_eq!(mac, [0x04, 0xE4, 0xB6, 0xB3, 0x90, 0x82]);
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[..6], &[0xFF; 6]);
        assert_eq!(&packet[96..], &mac);
        assert!(parse_mac("04:E4:B6").is_err());
        assert!(parse_mac("zz:E4:B6:B3:90:82").is_err());
    }
}
