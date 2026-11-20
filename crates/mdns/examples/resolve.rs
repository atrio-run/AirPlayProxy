//! Asks the local network for the address of a host, to try the crate from a computer.
//!
//! `resolve <name> <index of the network interface>`

use std::env;
use std::error::Error;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(host), Some(interface)) = (arguments.next(), arguments.next()) else {
        return Err("usage: resolve <name> <index of the network interface>".into());
    };

    let asked = Instant::now();
    let address = mdns::resolve(&host, interface.parse()?, Duration::from_secs(3))?;
    println!("{host} is {address}, answered in {:?}", asked.elapsed());
    Ok(())
}
