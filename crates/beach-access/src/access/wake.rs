//! Standard Wake-on-LAN magic packets, optionally sent by an SSH relay on the
//! destination LAN. Only validated numeric addresses enter remote commands.

use std::io::Read as _;
use std::net::{SocketAddrV4, UdpSocket};

use anyhow::{bail, Context as _};
use base64::Engine as _;

use super::{ssh, Shell, Wake, Worker};

pub(super) fn packet(mac: &str) -> anyhow::Result<[u8; 102]> {
    let separator = if mac.contains(':') { ':' } else { '-' };
    let parts: Vec<_> = mac.trim().split(separator).collect();
    if parts.len() != 6
        || parts
            .iter()
            .any(|part| part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        bail!("MAC-Adresse muss aus sechs Hexadezimalpaaren bestehen, etwa 00:11:22:33:44:55.");
    }
    let mut address = [0; 6];
    for (byte, part) in address.iter_mut().zip(parts) {
        *byte = u8::from_str_radix(part, 16)?;
    }
    if address == [0; 6] || address[0] & 1 != 0 {
        bail!("Bitte die individuelle MAC-Adresse der Netzwerkkarte eingeben.");
    }
    let mut packet = [0xff; 102];
    for chunk in packet[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&address);
    }
    Ok(packet)
}

pub(super) fn destination(value: &str) -> anyhow::Result<SocketAddrV4> {
    let address: SocketAddrV4 = value
        .trim()
        .parse()
        .context("Aufweckziel als IPv4-Adresse mit Port eingeben, etwa 192.168.1.255:9.")?;
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        bail!(
            "Ungültiges Aufweckziel. Bitte eine IPv4-Adresse und einen Port zwischen 1 und 65535 eingeben."
        );
    }
    Ok(address)
}

pub(super) fn send(worker: &Worker, config: &Wake) -> anyhow::Result<()> {
    let packet = packet(&config.mac)?;
    let destination = destination(&config.destination)?;
    if config.relays.is_empty() {
        worker.cancel.check()?;
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.set_broadcast(true)?;
        socket.send_to(&packet, destination)?;
        return Ok(());
    }
    let mut errors = Vec::new();
    for relay in &config.relays {
        worker.cancel.check()?;
        worker.progress(format!("Aufwecksignal über {} senden …", relay.ssh.address));
        let result = (|| -> anyhow::Result<()> {
            let session = ssh::login(worker, &relay.ssh, false)?
                .context("Keine SSH-Sitzung zum Zugangsgerät verfügbar.")?;
            worker.cancel.check()?;
            let mut channel = session.channel_session()?;
            channel.exec(&command(relay.shell, &packet, destination))?;
            let mut output = Vec::new();
            channel.by_ref().take(4096).read_to_end(&mut output)?;
            let mut errors = Vec::new();
            channel.stderr().take(4096).read_to_end(&mut errors)?;
            channel.wait_close()?;
            if channel.exit_status()? != 0 {
                bail!(
                    "Aufwecksignal konnte nicht gesendet werden: {}",
                    String::from_utf8_lossy(&errors)
                );
            }
            worker.cancel.check()?;
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{}: {error:#}", relay.ssh.address)),
        }
    }
    bail!(
        "Kein Zugangsgerät konnte das Aufwecksignal senden.\n{}",
        errors.join("\n")
    )
}

fn command(shell: Shell, packet: &[u8; 102], destination: SocketAddrV4) -> String {
    let hex: String = packet.iter().map(|byte| format!("{byte:02x}")).collect();
    let ip = destination.ip();
    let port = destination.port();
    match shell {
        Shell::Posix => {
            let python = format!("import socket;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.setsockopt(socket.SOL_SOCKET,socket.SO_BROADCAST,1);s.sendto(bytes.fromhex(\"{hex}\"),(\"{ip}\",{port}))");
            let perl = format!("socket(S,PF_INET,SOCK_DGRAM,getprotobyname(\"udp\")) or die $!;setsockopt(S,SOL_SOCKET,SO_BROADCAST,pack(\"i\",1)) or die $!;send(S,pack(\"H*\",\"{hex}\"),0,sockaddr_in({port},inet_aton(\"{ip}\"))) or die $!;");
            format!("if command -v python3 >/dev/null 2>&1; then python3 -c '{python}'; elif command -v perl >/dev/null 2>&1; then perl -MSocket -e '{perl}'; else echo 'Wake-on-LAN requires Python 3 or Perl on this SSH relay.' >&2; exit 127; fi")
        }
        Shell::PowerShell => {
            let script = format!("$ErrorActionPreference='Stop';$hex='{hex}';[byte[]]$packet=for($i=0;$i -lt $hex.Length;$i+=2){{[Convert]::ToByte($hex.Substring($i,2),16)}};$udp=[System.Net.Sockets.UdpClient]::new();try{{$udp.EnableBroadcast=$true;[void]$udp.Send($packet,$packet.Length,'{ip}',{port})}}finally{{$udp.Dispose()}}");
            let encoded = base64::engine::general_purpose::STANDARD.encode(
                script
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            format!("powershell.exe -NoProfile -NonInteractive -EncodedCommand {encoded}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_standard_magic_packet_and_rejects_invalid_addresses() {
        let bytes = packet("00:11:22:33:44:55").unwrap();
        assert_eq!(&bytes[..6], &[0xff; 6]);
        for chunk in bytes[6..].chunks_exact(6) {
            assert_eq!(chunk, [0, 17, 34, 51, 68, 85]);
        }
        assert_eq!(bytes, packet("00-11-22-33-44-55").unwrap());
        for value in [
            "",
            "00:11:22:33:44",
            "00:11:22:33:44:zz",
            "ff:ff:ff:ff:ff:ff",
            "00:00:00:00:00:00",
            "00:11:22:33:44:55;exit",
        ] {
            assert!(packet(value).is_err(), "{value}");
        }
        assert!(destination("127.0.0.1:9").is_ok());
        for value in ["host:9", "1.2.3.4:0", "0.0.0.0:9", "224.0.0.1:9"] {
            assert!(destination(value).is_err());
        }
    }

    #[test]
    fn powershell_command_contains_only_encoded_script() {
        let cmd = command(
            Shell::PowerShell,
            &packet("00:11:22:33:44:55").unwrap(),
            "192.168.1.255:9".parse().unwrap(),
        );
        let encoded = cmd.split_whitespace().next_back().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains("'192.168.1.255',9"));
        assert!(script.contains("$udp.EnableBroadcast=$true"));
    }
}
