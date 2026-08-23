use keeppeek::camera_catalog::{common_onvif_probe_ports, onvif_port_report};

pub fn run() -> anyhow::Result<()> {
    let report = onvif_port_report()?;
    println!(
        "camera catalog: {} records parsed from matching JSON and CSV entries",
        report.camera_count()
    );
    println!(
        "ONVIF-capable models: {}",
        report.onvif_capable_camera_count()
    );

    if report.has_catalog_port_evidence() {
        println!("catalog-declared ONVIF service ports:");
        for frequency in report.catalog_port_frequencies() {
            println!(
                "  {}: {} model(s)",
                frequency.port(),
                frequency.camera_count()
            );
        }
    } else {
        println!("catalog-declared ONVIF service ports: unavailable");
        println!(
            "curated ONVIF probe candidates (not catalog evidence): {}",
            common_onvif_probe_ports()
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(())
}
