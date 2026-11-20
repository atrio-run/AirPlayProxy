//! Asks the local network for the hosts that offer a service, to try the crate from a computer.
//!
//! `browse <service> <index of the network interface>`, such as `browse _raop._tcp.local 15`

use std::env;
use std::error::Error;
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(service), Some(interface)) = (arguments.next(), arguments.next()) else {
        return Err("usage: browse <service> <index of the network interface>".into());
    };

    let found = mdns::browse(&service, interface.parse()?, Duration::from_secs(2))?;
    for host in &found {
        let address = host.address.map_or("no address".to_owned(), |address| address.to_string());
        println!("{}: {}:{} ({address})", host.name, host.host, host.port);
    }
    println!("{} hosts offer {service}", found.len());
    Ok(())
}
