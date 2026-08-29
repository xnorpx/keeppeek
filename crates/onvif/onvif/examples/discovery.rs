extern crate onvif;
use onvif::discovery;

fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let devices = discovery::DiscoveryBuilder::default().discover().unwrap();
    for device in devices {
        println!("Device found: {device:?}");
    }
}
