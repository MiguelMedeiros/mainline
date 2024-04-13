use std::{thread, time::Duration};

use mainline::Dht;

use tracing::Level;
use tracing_subscriber;

fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    Dht::default();

    thread::sleep(Duration::from_secs(5));
}
