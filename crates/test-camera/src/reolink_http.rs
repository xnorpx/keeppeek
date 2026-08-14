use crate::media::{Codec, VideoSource};
use rouille::{Request, Response, Server};
use serde_json::{Value, json};
use std::{
    io::Read,
    net::SocketAddr,
    sync::{Arc, Mutex, mpsc::Sender},
    thread::JoinHandle,
};

pub struct ReolinkHttpServer {
    address: SocketAddr,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl ReolinkHttpServer {
    pub fn start(
        address: SocketAddr,
        username: String,
        password: String,
        main: VideoSource,
        sub: VideoSource,
        onvif_port: u16,
    ) -> anyhow::Result<Self> {
        let state = ReolinkHttpState {
            username,
            password,
            motion_enabled: Arc::new(Mutex::new(true)),
            main,
            sub,
            onvif_port,
            http_port: Arc::new(Mutex::new(0)),
        };
        let handler_state = state.clone();
        let server = Server::new(address, move |request| {
            handle_request(request, &handler_state)
        })
        .map_err(|error| anyhow::anyhow!("unable to bind fake Reolink HTTP API: {error}"))?;
        let address = server.server_addr();
        *state
            .http_port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = address.port();
        let (worker, stop) = server.stoppable();
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }
}

#[derive(Clone)]
struct ReolinkHttpState {
    username: String,
    password: String,
    motion_enabled: Arc<Mutex<bool>>,
    main: VideoSource,
    sub: VideoSource,
    onvif_port: u16,
    http_port: Arc<Mutex<u16>>,
}

impl Drop for ReolinkHttpServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle_request(request: &Request, state: &ReolinkHttpState) -> Response {
    if request.method() == "GET" && request.url() == "/" {
        return Response::html(
            "<!doctype html><html><head><title>Fake Reolink Camera</title></head><body><h1>Fake Reolink Camera</h1><p>Fake camera built-in UI</p></body></html>",
        );
    }
    let command = request.get_param("cmd").unwrap_or_default();
    let payload = request.data().map_or(Value::Null, |mut body| {
        let mut text = String::new();
        let _ = body.read_to_string(&mut text);
        serde_json::from_str(&text).unwrap_or(Value::Null)
    });
    let value = match command.as_str() {
        "Login" if credentials_match(&payload, &state.username, &state.password) => json!({
            "Token": { "name": "fake-reolink-token" }
        }),
        "Login" => return error_response(&command, "invalid credentials"),
        "GetDevInfo" => json!({
            "DevInfo": {
                "model": "RLC-Test",
                "firmVer": "fake-reo-1.0",
                "serial": "FAKE-REO-0001",
                "hardVer": "fake-reo-hardware"
            }
        }),
        "GetP2p" => json!({ "P2p": { "uid": "TESTCAMERA0001" } }),
        "GetNetPort" => json!({
            "NetPort": {
                "httpPort": *state.http_port.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
                "onvifPort": state.onvif_port
            }
        }),
        "GetLocalLink" => json!({ "LocalLink": { "mac": "02:00:00:00:00:42" } }),
        "GetEnc" => json!({
            "Enc": {
                "channel": 0,
                "audio": 1,
                "mainStream": stream_config(&state.main, 8192),
                "subStream": stream_config(&state.sub, 1024)
            }
        }),
        "GetAudioCfg" => json!({
            "AudioCfg": { "audioType": "aac", "sampleRate": 16000, "bitRate": 64 }
        }),
        "GetOsd" => json!({ "Osd": { "osdChannel": { "name": "Fake Reo-Proto" } } }),
        "GetImage" => json!({
            "Image": { "bright": 128, "contrast": 128, "saturation": 128, "sharpen": 128 }
        }),
        "GetIrLights" => json!({ "IrLights": { "state": "Auto" } }),
        "GetPtzPreset" => json!({ "PtzPreset": [] }),
        "GetAbility" => json!({
            "Ability": {
                "ptz": { "permit": 1 },
                "alarm": { "permit": 1 },
                "record": { "permit": 1 },
                "abilityChn": [{
                    "ptz": { "permit": 1 },
                    "audioCfg": { "permit": 1 },
                    "alarm": { "permit": 1 },
                    "recCfg": { "permit": 1 },
                    "ai": { "permit": 1 },
                    "image": { "permit": 1 },
                    "talkCfg": { "permit": 1 }
                }]
            }
        }),
        "GetMdState" => json!({
            "state": u8::from(*state.motion_enabled.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
        }),
        "SetAlarm" => match requested_motion_state(&payload) {
            Some(enabled) => {
                *state
                    .motion_enabled
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = enabled;
                json!({})
            }
            None => return error_response(&command, "missing motion enable value"),
        },
        "Logout" => json!({}),
        _ => return error_response(&command, "unsupported fake Reolink command"),
    };
    Response::json(&json!([{ "cmd": command, "code": 0, "value": value }]))
}

fn stream_config(source: &VideoSource, bitrate: u32) -> Value {
    json!({
        "bitRate": bitrate,
        "frameRate": source.fps,
        "gop": u32::from(source.fps) * 2,
        "height": source.height,
        "width": source.width,
        "profile": matches!(source.codec, Codec::H264).then_some("High"),
        "vType": match source.codec {
            Codec::H264 => "h264",
            Codec::H265 => "h265"
        }
    })
}

fn credentials_match(payload: &Value, username: &str, password: &str) -> bool {
    let Some(user) = payload
        .as_array()
        .and_then(|requests| requests.first())
        .and_then(|request| request.get("param"))
        .and_then(|param| param.get("User"))
    else {
        return false;
    };
    user.get("userName").and_then(Value::as_str) == Some(username)
        && user.get("password").and_then(Value::as_str) == Some(password)
}

fn requested_motion_state(payload: &Value) -> Option<bool> {
    payload
        .as_array()
        .and_then(|requests| requests.first())
        .and_then(|request| request.get("param"))
        .and_then(|param| param.get("Alarm"))
        .and_then(|alarm| alarm.get("enable"))
        .and_then(Value::as_u64)
        .map(|enabled| enabled != 0)
}

fn error_response(command: &str, detail: &str) -> Response {
    Response::json(&json!([{
        "cmd": command,
        "code": 1,
        "error": { "detail": detail, "rspCode": 1 }
    }]))
}
