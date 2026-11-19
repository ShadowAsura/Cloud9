use crate::Error;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
    time::Duration,
};

pub fn local_ip() -> IpAddr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("192.0.2.1:9")?;
            Ok(socket.local_addr()?.ip())
        })
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

pub fn free_port(ip: IpAddr) -> Result<u16, Error> {
    Ok(TcpListener::bind(SocketAddr::new(ip, 0))?
        .local_addr()?
        .port())
}

pub fn reachable(address: &str) -> Result<bool, Error> {
    let addresses = address.to_socket_addrs()?;
    Ok(addresses
        .take(4)
        .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()))
}

pub(crate) struct Mapping {
    gateway: igd_next::Gateway,
    pub address: SocketAddr,
    local: SocketAddr,
}

impl Mapping {
    pub fn create(local: SocketAddr) -> Result<Self, Error> {
        if !local.is_ipv4() || local.ip().is_loopback() {
            return Err(Error::Config("UPnP requires an IPv4 LAN address".into()));
        }
        let mut options = igd_next::SearchOptions::default();
        options.timeout = Some(Duration::from_secs(3));
        let gateway = igd_next::search_gateway(options)
            .map_err(|error| Error::Config(format!("UPnP gateway: {error}")))?;
        let address = gateway
            .get_any_address(
                igd_next::PortMappingProtocol::TCP,
                local,
                1800,
                "Cloud 9 Minecraft server",
            )
            .map_err(|error| Error::Config(format!("UPnP mapping: {error}")))?;
        Ok(Self {
            gateway,
            address,
            local,
        })
    }

    pub fn refresh(&self) -> Result<(), Error> {
        self.gateway
            .add_port(
                igd_next::PortMappingProtocol::TCP,
                self.address.port(),
                self.local,
                1800,
                "Cloud 9 Minecraft server",
            )
            .map_err(|error| Error::Config(format!("UPnP renewal: {error}")))
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if let Err(error) = self
            .gateway
            .remove_port(igd_next::PortMappingProtocol::TCP, self.address.port())
        {
            tracing::warn!(%error, "UPnP mapping could not be removed; lease will expire");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_local_tcp_listener() -> Result<(), Error> {
        let socket = TcpListener::bind("127.0.0.1:0")?;
        let address = socket.local_addr()?.to_string();
        assert!(reachable(&address)?);
        Ok(())
    }
}
