use crate::media::{Codec, VideoSource};
use rouille::{Request, Response, Server};
use std::{io::Read, net::SocketAddr, sync::mpsc::Sender, thread::JoinHandle};

#[derive(Clone)]
pub struct ProfileDescription {
    token: String,
    name: String,
    codec: Codec,
    width: u32,
    height: u32,
    fps: u8,
    stream_url: String,
}

impl ProfileDescription {
    pub(crate) fn from_source(
        token: &str,
        name: &str,
        source: VideoSource,
        stream_url: String,
    ) -> Self {
        Self {
            token: token.to_owned(),
            name: name.to_owned(),
            codec: source.codec,
            width: source.width,
            height: source.height,
            fps: source.fps,
            stream_url,
        }
    }
}

#[derive(Clone)]
pub struct CameraDescription {
    pub(crate) manufacturer: String,
    pub(crate) model: String,
    pub(crate) main: ProfileDescription,
    pub(crate) sub: ProfileDescription,
}

pub struct OnvifServer {
    address: SocketAddr,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl OnvifServer {
    pub(crate) fn start(address: SocketAddr, camera: CameraDescription) -> anyhow::Result<Self> {
        let server = Server::new(address, move |request| handle_request(request, &camera))
            .map_err(|error| anyhow::anyhow!("unable to start ONVIF test server: {error}"))?;
        let address = server.server_addr();
        let (worker, stop) = server.stoppable();
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub(crate) const fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for OnvifServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle_request(request: &Request, camera: &CameraDescription) -> Response {
    let payload = request.data().map_or_else(String::new, |mut data| {
        let mut payload = String::new();
        let _ = data.read_to_string(&mut payload);
        payload
    });
    let response = if payload.contains("GetServices") {
        get_services(request)
    } else if payload.contains("GetDeviceInformation") {
        get_device_information(camera)
    } else if payload.contains("GetHostname") {
        get_hostname()
    } else if payload.contains("GetScopes") {
        get_scopes()
    } else if payload.contains("GetProfiles") {
        get_profiles(camera)
    } else if payload.contains("GetStreamUri") {
        get_stream_uri(camera, &payload)
    } else {
        soap_fault("unsupported ONVIF request")
    };
    Response::text(response).with_additional_header("Content-Type", "application/soap+xml")
}

fn get_services(request: &Request) -> String {
    let address = request
        .header("Host")
        .map_or_else(|| "127.0.0.1".to_owned(), ToOwned::to_owned);
    soap(&format!(
        "<tds:GetServicesResponse><tds:Service><tds:Namespace>http://www.onvif.org/ver10/media/wsdl</tds:Namespace><tds:XAddr>http://{address}/onvif/media_service</tds:XAddr><tds:Capabilities/><tds:Version><tt:Major>1</tt:Major><tt:Minor>0</tt:Minor></tds:Version></tds:Service></tds:GetServicesResponse>"
    ))
}

fn get_device_information(camera: &CameraDescription) -> String {
    soap(&format!(
        "<tds:GetDeviceInformationResponse><tds:Manufacturer>{}</tds:Manufacturer><tds:Model>{}</tds:Model><tds:FirmwareVersion>test-camera</tds:FirmwareVersion><tds:SerialNumber>TESTCAMERA0001</tds:SerialNumber><tds:HardwareId>test-camera</tds:HardwareId></tds:GetDeviceInformationResponse>",
        camera.manufacturer, camera.model
    ))
}

fn get_hostname() -> String {
    soap(
        "<tds:GetHostnameResponse><tds:HostnameInformation><tt:FromDHCP>false</tt:FromDHCP><tt:Name>test-camera</tt:Name></tds:HostnameInformation></tds:GetHostnameResponse>",
    )
}

fn get_scopes() -> String {
    soap(
        "<tds:GetScopesResponse><tds:Scopes><tt:ScopeDef>Fixed</tt:ScopeDef><tt:ScopeItem>onvif://www.onvif.org/name/test-camera</tt:ScopeItem></tds:Scopes></tds:GetScopesResponse>",
    )
}

fn get_profiles(camera: &CameraDescription) -> String {
    soap(&format!(
        "<trt:GetProfilesResponse>{}{}</trt:GetProfilesResponse>",
        profile_xml(&camera.main),
        profile_xml(&camera.sub),
    ))
}

fn get_stream_uri(camera: &CameraDescription, payload: &str) -> String {
    let profile = if payload.contains("sub") {
        &camera.sub
    } else {
        &camera.main
    };
    soap(&format!(
        "<trt:GetStreamUriResponse><trt:MediaUri><tt:Uri>{}</tt:Uri><tt:InvalidAfterConnect>false</tt:InvalidAfterConnect><tt:InvalidAfterReboot>false</tt:InvalidAfterReboot><tt:Timeout>PT0S</tt:Timeout></trt:MediaUri></trt:GetStreamUriResponse>",
        profile.stream_url
    ))
}

fn profile_xml(profile: &ProfileDescription) -> String {
    let h264 = matches!(profile.codec, Codec::H264).then_some(
        "<tt:H264><tt:GovLength>30</tt:GovLength><tt:H264Profile>Baseline</tt:H264Profile></tt:H264>",
    );
    format!(
        "<trt:Profiles token=\"{}\"><tt:Name>{}</tt:Name><tt:VideoEncoderConfiguration token=\"{}-video\"><tt:Name>{} Video</tt:Name><tt:UseCount>1</tt:UseCount><tt:Encoding>{}</tt:Encoding><tt:Resolution><tt:Width>{}</tt:Width><tt:Height>{}</tt:Height></tt:Resolution><tt:Quality>5</tt:Quality><tt:RateControl><tt:FrameRateLimit>{}</tt:FrameRateLimit><tt:EncodingInterval>1</tt:EncodingInterval><tt:BitrateLimit>4096</tt:BitrateLimit></tt:RateControl>{}<tt:Multicast><tt:Address><tt:Type>IPv4</tt:Type><tt:IPv4Address>0.0.0.0</tt:IPv4Address></tt:Address><tt:Port>0</tt:Port><tt:TTL>0</tt:TTL><tt:AutoStart>false</tt:AutoStart></tt:Multicast><tt:SessionTimeout>PT0S</tt:SessionTimeout></tt:VideoEncoderConfiguration></trt:Profiles>",
        profile.token,
        profile.name,
        profile.token,
        profile.name,
        profile.codec.onvif_name(),
        profile.width,
        profile.height,
        profile.fps,
        h264.unwrap_or_default(),
    )
}

fn soap(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:tds=\"http://www.onvif.org/ver10/device/wsdl\" xmlns:trt=\"http://www.onvif.org/ver10/media/wsdl\" xmlns:tt=\"http://www.onvif.org/ver10/schema\"><s:Body>{body}</s:Body></s:Envelope>"
    )
}

fn soap_fault(reason: &str) -> String {
    soap(&format!(
        "<s:Fault><s:Reason><s:Text>{reason}</s:Text></s:Reason></s:Fault>"
    ))
}
