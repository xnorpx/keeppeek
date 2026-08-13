use rouille_multipart as multipart;
extern crate nickel;

use nickel::status::StatusCode;
use nickel::{Action, HttpRouter, MiddlewareResult, Nickel, Request, Response};
use std::io::{self, Write};

use multipart::mock::StdoutTee;
use multipart::server::nickel::MultipartBody;
use multipart::server::{Entries, SaveResult};

#[expect(
    clippy::result_large_err,
    reason = "Nickel fixes the middleware error type"
)]
fn handle_multipart<'mw>(req: &mut Request, mut res: Response<'mw>) -> MiddlewareResult<'mw> {
    match (*req).multipart_body() {
        Some(mut multipart) => match multipart.save().temp() {
            SaveResult::Full(entries) => process_entries(res, entries),

            SaveResult::Partial(entries, e) => {
                println!("Partial errors ... {e:?}");
                process_entries(res, entries.keep_partial())
            }

            SaveResult::Error(e) => {
                println!("There are errors in multipart POSTing ... {e:?}");
                res.set(StatusCode::InternalServerError);
                res.send(format!("Server could not handle multipart POST! {e:?}"))
            }
        },
        None => {
            res.set(StatusCode::BadRequest);
            res.send("Request seems not was a multipart request")
        }
    }
}

/// Processes saved entries from multipart request.
/// Returns an OK response or an error.
#[expect(
    clippy::result_large_err,
    reason = "Nickel fixes the middleware error type"
)]
fn process_entries<'mw>(res: Response<'mw>, entries: Entries) -> MiddlewareResult<'mw> {
    let stdout = io::stdout();
    let mut res = res.start()?;
    if let Err(e) = entries.write_debug(StdoutTee::new(&mut res, &stdout)) {
        writeln!(res, "Error while reading entries: {e}").expect("writeln");
    }

    Ok(Action::Halt(res))
}

fn main() {
    let mut srv = Nickel::new();

    srv.post("/multipart_upload/", handle_multipart);

    // Start this example via:
    //
    // `cargo run --example nickel --features nickel`
    //
    // And - if you are in the root of this repository - do an example
    // upload via:
    //
    // `curl -F file=@LICENSE 'http://localhost:6868/multipart_upload/'`
    srv.listen("127.0.0.1:6868").expect("Failed to bind server");
}
