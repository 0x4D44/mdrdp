//! Bind the session's advertised side channels before video can accept a client.

use std::io;
use std::net::{Ipv4Addr, TcpListener};

pub(crate) struct BoundChannels {
    pub(crate) input: TcpListener,
    pub(crate) sparse: TcpListener,
    pub(crate) aux: Option<TcpListener>,
}

impl BoundChannels {
    pub(crate) fn bind(input_port: u16, sparse_port: u16, aux_port: u16) -> io::Result<Self> {
        let input = TcpListener::bind((Ipv4Addr::LOCALHOST, input_port))?;
        let sparse = TcpListener::bind((Ipv4Addr::LOCALHOST, sparse_port))?;
        let aux = if aux_port == 0 {
            None
        } else {
            Some(TcpListener::bind((Ipv4Addr::LOCALHOST, aux_port))?)
        };
        Ok(Self { input, sparse, aux })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unused_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn all_required_channels_are_bound_together() {
        let channels = BoundChannels::bind(0, 0, 0).unwrap();
        assert_ne!(channels.input.local_addr().unwrap().port(), 0);
        assert_ne!(channels.sparse.local_addr().unwrap().port(), 0);
        assert!(channels.aux.is_none());

        let channels = BoundChannels::bind(0, 0, unused_port()).unwrap();
        assert!(channels.aux.is_some());
    }

    #[test]
    fn an_aux_bind_failure_releases_the_input_bind_too() {
        let occupied_aux = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let aux_port = occupied_aux.local_addr().unwrap().port();
        let input_port = unused_port();
        assert!(BoundChannels::bind(input_port, 0, aux_port).is_err());
        TcpListener::bind((Ipv4Addr::LOCALHOST, input_port))
            .expect("the failed channel set must not retain its input listener");
    }
}
