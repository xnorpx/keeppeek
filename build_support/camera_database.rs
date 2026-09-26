use std::{io, thread, time::Duration};

const CAMERA_DATABASE_DOWNLOAD_ATTEMPTS: usize = 3;

pub fn download_camera_database_archive(url: &str) -> io::Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut last_error = None;
    for attempt in 1..=CAMERA_DATABASE_DOWNLOAD_ATTEMPTS {
        let response = match agent.get(url).call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(status)) if !(500..=599).contains(&status) => {
                return Err(io::Error::other(format!(
                    "camera database download returned HTTP {status}"
                )));
            }
            Err(error) => {
                last_error = Some(format!("failed to download camera database: {error}"));
                if attempt < CAMERA_DATABASE_DOWNLOAD_ATTEMPTS {
                    eprintln!(
                        "camera database download attempt {attempt} failed: {error}; retrying"
                    );
                    thread::sleep(Duration::from_secs(attempt as u64));
                    continue;
                }
                break;
            }
        };
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "camera database download returned HTTP {}",
                response.status()
            )));
        }
        let mut body = response.into_body();
        match body.read_to_vec() {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                last_error = Some(format!("failed to read camera database: {error}"));
                if attempt < CAMERA_DATABASE_DOWNLOAD_ATTEMPTS {
                    eprintln!(
                        "camera database download attempt {attempt} failed while reading: {error}; retrying"
                    );
                    thread::sleep(Duration::from_secs(attempt as u64));
                }
            }
        }
    }
    Err(io::Error::other(last_error.unwrap_or_else(|| {
        "failed to download camera database".to_owned()
    })))
}
